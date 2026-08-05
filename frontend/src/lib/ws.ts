// Typed client for the mimodel session WebSocket protocol.
// Wire shapes are pinned by the plan's Task 2.2 Interfaces block — keep in
// sync with src/server/ws.rs exactly.

export type ClientMsg =
  | { type: 'prompt'; text: string; part_refs: string[]; lib_refs: string[] }
  | { type: 'approve_phase' }
  | { type: 'advance' }
  | { type: 'go_back'; target: 'spec' | 'build' }
  | { type: 'cancel_stream' };

export type ServerMsg =
  | {
      type: 'snapshot';
      phase: string;
      approved: boolean;
      conversation: { role: string; content: string }[];
      iterations: number[];
      spec: string | null;
    }
  | { type: 'stream_delta'; text: string }
  | { type: 'tool_call'; name: string; detail: string }
  | { type: 'phase_state'; phase: string; approved: boolean }
  | { type: 'iteration_added'; n: number }
  | { type: 'build_progress'; component: string; status: 'building' | 'done' | 'failed' }
  | { type: 'error'; message: string };

export interface SessionHandlers {
  onMessage(m: ServerMsg): void;
  onOpen?(): void;
  onClose?(): void;
}

export interface SessionClient {
  send(m: ClientMsg): void;
  close(): void;
}

const INITIAL_BACKOFF_MS = 250;
const MAX_BACKOFF_MS = 5000;

export function connectSession(projectId: string, handlers: SessionHandlers): SessionClient {
  let ws: WebSocket | null = null;
  let closedByUser = false;
  let backoff = INITIAL_BACKOFF_MS;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  const pending: ClientMsg[] = [];

  const url = () => {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    return `${proto}//${location.host}/api/session?project=${encodeURIComponent(projectId)}`;
  };

  function flushPending() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    while (pending.length > 0) {
      const m = pending.shift()!;
      ws.send(JSON.stringify(m));
    }
  }

  function scheduleReconnect() {
    if (closedByUser) return;
    if (reconnectTimer !== null) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      backoff = Math.min(backoff * 2, MAX_BACKOFF_MS);
      open();
    }, backoff);
  }

  function open() {
    if (closedByUser) return;
    const socket = new WebSocket(url());
    ws = socket;

    socket.addEventListener('open', () => {
      backoff = INITIAL_BACKOFF_MS;
      flushPending();
      handlers.onOpen?.();
    });

    socket.addEventListener('message', (ev) => {
      let parsed: ServerMsg;
      try {
        parsed = JSON.parse(ev.data as string) as ServerMsg;
      } catch {
        return;
      }
      handlers.onMessage(parsed);
    });

    socket.addEventListener('close', () => {
      handlers.onClose?.();
      if (!closedByUser) scheduleReconnect();
    });

    socket.addEventListener('error', () => {
      socket.close();
    });
  }

  open();

  return {
    send(m: ClientMsg) {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(m));
      } else {
        pending.push(m);
      }
    },
    close() {
      closedByUser = true;
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      ws?.close();
    }
  };
}
