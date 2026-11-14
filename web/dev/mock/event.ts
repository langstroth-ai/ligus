// Dev-only stand-in for @tauri-apps/api/event (see vite.mock.config.ts).
// Never imported by the production build.
export type UnlistenFn = () => void;
export type Event<T> = { event: string; id: number; payload: T };
type Handler = (e: Event<unknown>) => void;

const handlers = new Map<string, Set<Handler>>();

export async function listen<T>(event: string, handler: (e: Event<T>) => void): Promise<UnlistenFn> {
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(handler as Handler);
  handlers.set(event, set);
  return () => set.delete(handler as Handler);
}

export async function once<T>(event: string, handler: (e: Event<T>) => void): Promise<UnlistenFn> {
  const un = await listen<T>(event, (e) => {
    un();
    handler(e);
  });
  return un;
}

export async function emit(event: string, payload?: unknown): Promise<void> {
  handlers.get(event)?.forEach((h) => h({ event, id: 0, payload }));
}

// Let the screenshot driver fire backend events (e.g. tray navigation).
(window as unknown as { __mockEmit?: typeof emit }).__mockEmit = emit;
