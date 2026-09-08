'use strict';
let activeChatJob = null;
let chatTimer = null;
let activeChatUserMessage = null;
let chatBefore = null, chatAfter = null, messagesCache = [];
let checklistUrl = '/api/checklist', checklistCache = null;
const conversation = 'pet';
const writeHeaders = {'Content-Type':'application/json', 'X-Journal-Request':'1'};
function chatBusy(busy) {
  $('chat-send').disabled = busy;
  $('restart-check').disabled = busy;
  $('chat-cancel').hidden = !busy;
}
function drawMessages(messages) {
  const target = $('chat-messages');
  target.replaceChildren();
  messages.forEach(message => {
    const article = el('article', undefined, `chat-message ${message.role}`);
    article.append(el('h3', message.role === 'user' ? '질문' : '답변'), el('div', message.text, 'body'));
    target.append(article);
  });
  target.scrollTop = target.scrollHeight;
}
async function chatHistory(after = null, before = null) {
  const query = new URLSearchParams({conversation_id:conversation});
  if (after !== null) query.set('after', after);
  if (before !== null) query.set('before', before);
  const result = await api(`/api/chat/history?${query}`);
  const merged = new Map(messagesCache.map(message => [message.id, message]));
  (result.messages || []).forEach(message => merged.set(message.id, message));
  messagesCache = [...merged.values()].sort((a,b) => a.id-b.id);
  if (after === null) chatBefore = result.next_before;
  if (before === null) chatAfter = result.next_after;
  $('chat-older').hidden = !chatBefore;
  $('chat-next').hidden = !chatAfter;
  drawMessages(messagesCache);
  return result.messages || [];
}
function drawChecklist(value) {
  if (!value || !Array.isArray(value.items)) return;
  const target = $('checklist-items');
  target.replaceChildren();
  const context = value.context || {};
  const run = context.current_run || context.last_run;
  const time = run?.started_at ? new Date(run.started_at).toLocaleString('ko-KR') : '앱 실행 시각 미확인';
  $('checklist-context').textContent = `${time} 이후 요청 ${context.since_start_count || 0}개 · 이전 확인 ${context.carryover_count || 0}개 · 시점 미확인 ${context.timestamp_unknown_count || 0}개`;
  if (context.artifact_states?.includes('unverified')) $('checklist-context').textContent += ' · 빌드 기록과 현재 파일의 일치 여부를 다시 확인해야 합니다.';
  const groups = value.groups || {built_not_running:'새 빌드에서 확인할 것',not_built:'아직 빌드되지 않은 것',implementation_unverified:'구현 확인이 필요한 것',carryover:'이전부터 남은 확인'};
  Object.entries(groups).forEach(([state, label]) => {
    const items = value.items.filter(item => item.buildstate === state);
    if (!items.length) return;
    const section = el('section', undefined, 'checklist-group');
    section.append(el('h3', `${label} · ${items.length}`));
    items.forEach(item => {
      const detail = el('details', undefined, 'checklist-item');
      detail.append(el('summary', item.title));
      const steps = el('ol');
      (item.steps || []).forEach(step => steps.append(el('li', step)));
      detail.append(steps);
      if (item.uncertain) detail.append(el('p', '포함 여부나 실제 동작은 아직 확인이 필요합니다.', 'notice'));
      (item.context_notes || []).forEach(note => detail.append(el('p', note, 'notice')));
      if (item.source_request_ids?.length) {
        const button = el('button', '요청 근거 보기');
        button.type = 'button';
        button.addEventListener('click', () => { $('records').open = true; select(item.source_request_ids[0]); });
        detail.append(button);
      }
      section.append(detail);
    });
    target.append(section);
  });
  if (!value.items.length) target.append(el('p', '확인할 항목이 아직 없습니다. 앱 실행 정보와 요청 수집 상태를 확인해 주세요.', 'empty'));
  if (value.coverage?.fallback_request_count) target.append(el('p', `의미 정리를 끝내지 못한 요청 ${value.coverage.fallback_request_count}개도 기본 확인 항목으로 보존했습니다.`, 'notice'));
  $('checklist-more').hidden = value.next_offset === null || value.next_offset === undefined;
  $('checklist-more').textContent = `확인 목록 더 보기 (${value.items.length}/${value.total_items || value.items.length})`;
  $('checklist-supplement').hidden = !value.supplementary_count;
  $('checklist-supplement').textContent = value.view === 'supplementary' ? '주요 기능 목록으로' : `추가 근거 ${value.supplementary_count}건 보기`;
}
async function loadChecklist(url = '/api/checklist', append = false) {
  checklistUrl = url;
  const parsed = new URL(url, location.origin);
  if (append && checklistCache?.next_offset !== null) parsed.searchParams.set('offset', checklistCache.next_offset);
  const page = await api(parsed.pathname + parsed.search);
  checklistCache = append && checklistCache ? {...page, items:[...checklistCache.items,...page.items]} : page;
  drawChecklist(checklistCache);
}
async function pollChat() {
  const id = activeChatJob;
  if (!id) return;
  try {
    const job = await api(`/api/chat/jobs/${encodeURIComponent(id)}`);
    if (id !== activeChatJob) return;
    $('chat-progress').textContent = job.progress ? `요청 묶음 ${job.progress.completed_batches}/${job.progress.total_batches} 확인 중` : job.status === 'queued' ? '앞선 답변이 끝나면 이어서 확인합니다.' : job.status === 'running' ? '앱 실행 이후 요청과 실제 빌드 근거를 확인하고 있습니다.' : job.partial ? '일부 정리는 기본 확인 항목으로 남겼습니다.' : job.status === 'cancelled' ? '답변을 멈췄습니다. 질문 기록은 남아 있습니다.' : job.status === 'failed' ? job.text || '답변을 만들지 못했습니다.' : '확인 목록을 정리했습니다.';
    if (['completed','failed','cancelled'].includes(job.status)) {
      const after = activeChatUserMessage === null ? null : activeChatUserMessage - 1;
      activeChatJob = null; activeChatUserMessage = null; chatBusy(false);
      await chatHistory(after);
      if (job.checklist_url && job.status !== 'cancelled') await loadChecklist(job.checklist_url);
      return;
    }
    chatTimer = setTimeout(pollChat, 800);
  } catch (e) {
    $('chat-progress').textContent = '연결을 다시 확인하고 있습니다. 질문 기록은 보존됩니다.';
    chatTimer = setTimeout(pollChat, 2500);
  }
}
async function submitChat(text) {
  if (activeChatJob || !text.trim()) return;
  chatBusy(true);
  try {
    const job = await api('/api/chat', {method:'POST', headers:writeHeaders, body:JSON.stringify({text, conversation_id:conversation, client_request_id:crypto.randomUUID()})});
    activeChatJob = job.job_id;
    activeChatUserMessage = job.user_message_id;
    $('chat-input').value = '';
    await chatHistory();
    pollChat();
  } catch (e) { chatBusy(false); $('chat-progress').textContent = '질문을 보내지 못했습니다. 입력은 남아 있으니 잠시 후 다시 보내 주세요.'; }
}
$('chat-form').addEventListener('submit', event => {event.preventDefault(); submitChat($('chat-input').value);});
$('restart-check').addEventListener('click', () => submitChat('나 재시작하면 뭐 확인해야 돼? 마지막 앱 실행 이후 요청과 실제 수정·빌드 근거를 종합해서 확인 목록을 알려줘.'));
$('chat-cancel').addEventListener('click', async () => {
  if (!activeChatJob) return;
  const id = activeChatJob;
  try { await api(`/api/chat/jobs/${encodeURIComponent(id)}`, {method:'DELETE', headers:writeHeaders, body:'{}'}); clearTimeout(chatTimer); pollChat(); }
  catch (e) { $('chat-progress').textContent = '취소 요청을 전달하지 못했습니다. 다시 눌러 주세요.'; }
});
$('chat-older').addEventListener('click', () => chatHistory(null, chatBefore));
$('chat-next').addEventListener('click', () => chatHistory(chatAfter));
$('checklist-more').addEventListener('click', () => loadChecklist(checklistUrl, true));
$('checklist-supplement').addEventListener('click', () => {
  const url = new URL(checklistUrl, location.origin);
  url.searchParams.delete('offset');
  if (checklistCache?.view === 'supplementary') url.searchParams.delete('view');
  else url.searchParams.set('view', 'supplementary');
  loadChecklist(url.pathname + url.search);
});
Promise.all([chatHistory(), loadChecklist()]).then(([messages]) => {
  if (messages.at(-1)?.role === 'user') {activeChatJob = messages.at(-1).job_id; activeChatUserMessage = messages.at(-1).id; chatBusy(true); pollChat();}
}).catch(() => {$('chat-progress').textContent = '채팅 서비스를 준비하고 있습니다.';});
