import { readSnapshot, type Snapshot } from './snapshot.ts';
export type Pull = () => Promise<unknown>;
export type Cache = { getItem(key: string): string | null; setItem(key: string, value: string): void | Promise<void>; removeItem(key: string): void | Promise<void> };
export type State = { mode: 'disconnected' | 'sample' | 'live'; snapshot: Snapshot | null; receivedAt: number | null; refreshing: boolean; error: string | null; storageWarning: boolean };
export class Companion {
  state: State = { mode: 'disconnected', snapshot: null, receivedAt: null, refreshing: false, error: null, storageWarning: false };
  private storage: Cache;
  private clock: () => number;
  private generation = 0;
  private writes: Promise<void> = Promise.resolve();
  private persist(work: () => void | Promise<void>): Promise<void> {
    this.writes = this.writes.catch(() => {}).then(work);
    return this.writes;
  }
  constructor(storage: Cache, clock: () => number) {
    this.storage = storage; this.clock = clock;
    try {
      const raw = storage.getItem('m-dash-sample-v1');
      if (raw) {
        const saved = JSON.parse(raw);
        if (saved.mode !== 'sample' || !Number.isSafeInteger(saved.receivedAt) || saved.receivedAt > clock() + 300) throw new Error();
        this.state.snapshot = readSnapshot(saved.snapshot, clock());
        this.state.receivedAt = saved.receivedAt;
        this.state.mode = 'sample';
      }
    } catch { this.state.storageWarning = true; }
  }
  async useLive(snapshot: unknown, receivedAt: number | null): Promise<void> {
    const admitted = snapshot === null ? null : readSnapshot(snapshot, this.clock());
    await this.clear(); this.state.mode = 'live'; this.state.snapshot = admitted; this.state.receivedAt = receivedAt;
  }
  async useSample(): Promise<void> { await this.clear(); this.state.mode = 'sample'; }
  async clear(): Promise<void> {
    this.generation++;
    this.state = { mode: 'disconnected', snapshot: null, receivedAt: null, refreshing: false, error: null, storageWarning: false };
    try { await this.persist(() => this.storage.removeItem('m-dash-sample-v1')); } catch { this.state.storageWarning = true; }
  }
  async refresh(pull: Pull = pullCloudflare): Promise<void> {
    if (this.state.refreshing) return;
    const generation = this.generation;
    this.state.refreshing = true;
    let received = false;
    try {
      const raw = await pull();
      if (generation !== this.generation) return;
      received = true;
      if (this.state.mode === 'disconnected') { this.state.error = 'not-configured'; return; }
      const snapshot = readSnapshot(raw, this.clock());
      const previous = this.state.snapshot;
      if (previous && (snapshot.sourceId !== previous.sourceId || snapshot.sequence < previous.sequence || snapshot.publishedAt < previous.publishedAt || (snapshot.sequence === previous.sequence && JSON.stringify(snapshot) !== JSON.stringify(previous)))) throw new Error('Invalid snapshot');
      this.state.snapshot = snapshot;
      this.state.receivedAt = this.clock();
      this.state.error = null;
      try {
        const saved = JSON.stringify({ mode: 'sample', snapshot, receivedAt: this.state.receivedAt });
        if (this.state.mode === 'sample') await this.persist(() => this.storage.setItem('m-dash-sample-v1', saved));
        this.state.storageWarning = false;
      } catch { this.state.storageWarning = true; }
    } catch { if (generation === this.generation) this.state.error = this.state.mode === 'disconnected' ? 'not-configured' : received ? 'invalid-snapshot' : 'offline'; }
    finally { if (generation === this.generation) this.state.refreshing = false; }
  }
}
export async function pullCloudflare(): Promise<unknown> { throw new Error('not-configured'); }
