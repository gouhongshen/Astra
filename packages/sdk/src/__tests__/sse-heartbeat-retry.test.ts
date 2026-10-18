import { afterEach, expect, test, vi } from 'vitest';
import { SSEClient } from '../sse-client';
import type { StreamEvent } from '../types';

afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });

function stalledResponse(signal: AbortSignal, terminal = false): Response {
  return new Response(new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(terminal
        ? 'data: {"type":"run_finished"}\n\n'
        : 'data: {"type":"text_delta","content":"partial"}\n\n'));
      signal.addEventListener('abort', () => controller.error(new DOMException('Aborted', 'AbortError')), { once: true });
    },
  }));
}

test('heartbeat timeout reconnects after backoff and emits only one retryable error', async () => {
  vi.useFakeTimers();
  const fetch = vi.fn().mockImplementationOnce((_url, init) => Promise.resolve(stalledResponse(init.signal)))
    .mockImplementation(() => Promise.resolve(new Response('data: {"type":"run_finished"}\n\n')));
  vi.stubGlobal('fetch', fetch);
  const events: StreamEvent[] = [];
  const client = new SSEClient({
    url: 'https://example.test/stream', heartbeatTimeoutMs: 100, retryDelayMs: 1_000,
    maxRetries: 1, requireTerminalEvent: true, onEvent: (event) => events.push(event),
  });
  const pending = client.connect();
  await vi.advanceTimersByTimeAsync(100);
  expect(events.filter((event) => event.type === 'error')).toEqual([
    { type: 'error', message: 'Connection timed out after 100ms without heartbeat', retryable: true },
  ]);
  await vi.advanceTimersByTimeAsync(999);
  expect(fetch).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1);
  await pending;
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(events.at(-1)).toEqual({ type: 'run_finished' });
  expect(events.filter((event) => event.type === 'error')).toHaveLength(1);
  expect(vi.getTimerCount()).toBe(0);
  client.close();
});

for (const maxRetries of [0, 1]) {
  test(`heartbeat timeout respects maxRetries=${maxRetries} and marks exhaustion`, async () => {
    vi.useFakeTimers();
    const fetch = vi.fn().mockImplementation((_url, init) => Promise.resolve(stalledResponse(init.signal)));
    vi.stubGlobal('fetch', fetch);
    const events: StreamEvent[] = [];
    const client = new SSEClient({
      url: 'https://example.test/stream', heartbeatTimeoutMs: 100, retryDelayMs: 10,
      maxRetries, onEvent: (event) => events.push(event),
    });
    const pending = client.connect();
    await vi.runAllTimersAsync();
    await pending;
    expect(fetch).toHaveBeenCalledTimes(maxRetries + 1);
    const errors = events.filter((event) => event.type === 'error');
    expect(errors).toHaveLength(maxRetries + 1);
    expect(errors.at(-1)).toMatchObject({ retryable: false });
    expect(vi.getTimerCount()).toBe(0);
    client.close();
  });
}

for (const cancel of ['close', 'external-abort']) {
  test(`${cancel} interrupts backoff after heartbeat timeout`, async () => {
    vi.useFakeTimers();
    const controller = new AbortController();
    const fetch = vi.fn().mockImplementation((_url, init) => Promise.resolve(stalledResponse(init.signal)));
    vi.stubGlobal('fetch', fetch);
    const client = new SSEClient({
      url: 'https://example.test/stream', signal: controller.signal,
      heartbeatTimeoutMs: 100, retryDelayMs: 10_000, maxRetries: 2, onEvent: () => {},
    });
    let settled = false;
    const pending = client.connect().finally(() => { settled = true; });
    await vi.advanceTimersByTimeAsync(100);
    expect(settled).toBe(false);
    if (cancel === 'close') client.close(); else controller.abort();
    await vi.advanceTimersByTimeAsync(1);
    expect(settled).toBe(true);
    expect(vi.getTimerCount()).toBe(0);
    await pending;
    await vi.runAllTimersAsync();
    expect(fetch).toHaveBeenCalledTimes(1);
    client.close();
  });
}

test('a terminal event on an open body suppresses heartbeat failure and retry', async () => {
  vi.useFakeTimers();
  const fetch = vi.fn().mockImplementation((_url, init) => Promise.resolve(stalledResponse(init.signal, true)));
  vi.stubGlobal('fetch', fetch);
  const events: StreamEvent[] = [];
  const client = new SSEClient({
    url: 'https://example.test/stream', heartbeatTimeoutMs: 100, retryDelayMs: 10,
    maxRetries: 1, requireTerminalEvent: true, onEvent: (event) => events.push(event),
  });
  const pending = client.connect();
  await vi.advanceTimersByTimeAsync(1_000);
  expect(events).toEqual([{ type: 'run_finished' }]);
  expect(fetch).toHaveBeenCalledTimes(1);
  client.close();
  await pending;
  expect(vi.getTimerCount()).toBe(0);
});
