from pathlib import Path
import argparse, os, subprocess, json, time, tempfile, concurrent.futures, http.client, statistics, hashlib
parser = argparse.ArgumentParser(description='Measure CPU and HTTP payloads using isolated transcript fixture hosts.')
parser.add_argument('--before', type=Path, required=True)
parser.add_argument('--after', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
p = args.output.resolve()
p.mkdir(parents=True, exist_ok=True)
results = []
digests = {}
for label, binary in [('before', args.before.resolve()), ('after', args.after.resolve())]:
    for size in [512 * 1024, 2 * 1024 * 1024]:
        root = Path(tempfile.mkdtemp(prefix='khttp-'))
        env = {k: v for k, v in os.environ.items() if k in ('HOME', 'USER', 'PATH', 'LANG', 'SHELL')}
        env.update(KASATERM_SETTINGS_FILE=str(root / 'settings.json'), KASATERM_SESSION_FILE=str(root / 'session.json'), KASATERM_WINDOW_FILE=str(root / 'window.json'), KASATERM_DEVICE_FILE=str(root / 'device.json'), KASATERM_MACHINES='[]', KASATERM_COLLAB_ROOT=str(root / 'collab'), KASATERM_KASANET='off', KASATERM_KASANET_KEY=str(root / 'key'), KASATERM_MACHINE_ID_FILE=str(root / 'machine'), KASATERM_MOBILE_USERS=str(root / 'users.json'), TMPDIR=str(root))
        with (p / f'bench-{label}-{size}.log').open('w') as log:
            proc = subprocess.Popen([str(binary), str(size)], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True)
            try:
                info = json.loads(proc.stdout.readline())
                port = info['port']
                raw_bytes = info['raw_bytes']

                def request(offset=0):
                    c = http.client.HTTPConnection('127.0.0.1', port, timeout=60)
                    start = time.perf_counter()
                    try:
                        c.request('GET', f'/transcript-raw?surface=%250&offset={offset}&source=fixture.jsonl')
                        r = c.getresponse()
                        body = r.read()
                        latency = time.perf_counter() - start
                        assert r.status == 200
                        value = json.loads(body)
                        assert value['ok'] and value['offset'] == raw_bytes and (value['source'] == 'fixture.jsonl')
                        assert len(value['raw'].encode()) == (raw_bytes if offset == 0 else 0)
                        return (latency, len(body), hashlib.sha256(value['raw'].encode()).hexdigest())
                    finally:
                        c.close()
                request()
                time.sleep(0.3)
                for incremental, count in [(False, 32), (True, 64)]:
                    proc.stdin.write('start\n')
                    proc.stdin.flush()
                    time.sleep(0.03)
                    start = time.perf_counter()
                    with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                        rows = list(pool.map(lambda _: request(raw_bytes if incremental else 0), range(count)))
                    elapsed = time.perf_counter() - start
                    hashes = {r[2] for r in rows}
                    assert len(hashes) == 1
                    digest = hashes.pop()
                    key = (size, incremental)
                    assert digest == digests.setdefault(key, digest), 'response content changed'
                    proc.stdin.write('stop\n')
                    proc.stdin.flush()
                    measured = json.loads(proc.stdout.readline())
                    row = {'variant': label, 'raw_bytes': raw_bytes, 'incremental': incremental, 'requests': count, 'server_cpu_seconds': measured['cpu_seconds'], 'server_calls': measured['calls'], 'wall_seconds': elapsed, 'average_cpu_percent': 100 * measured['cpu_seconds'] / elapsed, 'bytes_sent': sum((r[1] for r in rows)), 'median_ms': statistics.median((r[0] for r in rows)) * 1000, 'raw_sha256': digest}
                    assert measured['calls'] == count, row
                    results.append(row)
                    print(json.dumps(row), flush=True)
            finally:
                if proc.poll() is None:
                    try:
                        proc.communicate('quit\n', timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.terminate()
                        proc.communicate(timeout=5)
(p / 'bench-results.json').write_text(json.dumps(results, indent=2))
