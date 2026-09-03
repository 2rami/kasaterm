export interface FrameScheduler {
  request(callback: () => void): number;
  cancel(id: number): void;
}

type Sender = (value: string) => Promise<unknown>;
type QueuedPreview = { value: string; send: Sender };

export class LatestCommitCoordinator {
  private preview: Sender;
  private commit: Sender;
  private readonly frames: FrameScheduler;
  private pendingPreview: QueuedPreview | null = null;
  private previewInFlight: Promise<void> | null = null;
  private frame: number | null = null;
  private flushing = false;
  private active = true;
  private commitGeneration = 0;
  private commitChain: Promise<void> = Promise.resolve();

  constructor(
    preview: Sender,
    commit: Sender,
    frames: FrameScheduler,
  ) {
    this.preview = preview;
    this.commit = commit;
    this.frames = frames;
  }

  setSenders(preview: Sender, commit: Sender) {
    this.preview = preview;
    this.commit = commit;
  }

  pushPreview(value: string) {
    this.pendingPreview = { value, send: this.preview };
    this.schedulePreview();
  }

  commitLatest(value: string): Promise<void> {
    const generation = ++this.commitGeneration;
    const commit = this.commit;
    this.commitChain = this.commitChain
      .catch(() => undefined)
      .then(async () => {
        await this.flushPreviews();
        if (generation === this.commitGeneration) await commit(value);
      })
      .catch(() => undefined);
    return this.commitChain;
  }

  dispose() {
    this.active = false;
    if (this.frame !== null) this.frames.cancel(this.frame);
    this.frame = null;
    this.pendingPreview = null;
  }

  private schedulePreview() {
    if (!this.active || this.flushing || this.previewInFlight || this.frame !== null) return;
    this.frame = this.frames.request(() => {
      this.frame = null;
      const pending = this.pendingPreview;
      this.pendingPreview = null;
      if (pending === null) return;
      const sent = pending.send(pending.value).catch(() => undefined).then(() => undefined);
      this.previewInFlight = sent;
      void sent.finally(() => {
        if (this.previewInFlight === sent) this.previewInFlight = null;
        if (this.pendingPreview !== null) this.schedulePreview();
      });
    });
  }

  private async flushPreviews() {
    this.flushing = true;
    if (this.frame !== null) {
      this.frames.cancel(this.frame);
      this.frame = null;
    }
    try {
      await this.previewInFlight;
      while (this.pendingPreview !== null) {
        const pending = this.pendingPreview;
        this.pendingPreview = null;
        await pending.send(pending.value).catch(() => undefined);
      }
    } finally {
      this.flushing = false;
      if (this.pendingPreview !== null) this.schedulePreview();
    }
  }
}
