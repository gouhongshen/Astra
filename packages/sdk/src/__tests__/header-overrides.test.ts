import { afterEach, expect, test, vi } from 'vitest';
import { AstraClient } from '../client';
import { headersInitToRecord } from '../http';
import { SSEClient } from '../sse-client';

afterEach(() => vi.unstubAllGlobals());

for (const casing of ['Authorization', 'authorization', 'AUTHORIZATION']) {
  test(`REST access token replaces configured ${casing}`, async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      user_id: 'synthetic-user', username: 'test',
    }), { headers: { 'Content-Type': 'application/json' } }));
    vi.stubGlobal('fetch', fetch);
    const client = new AstraClient({
      baseUrl: 'https://example.test', accessToken: 'current-token',
      headers: { [casing]: 'Bearer old-token' },
    });
    await client.getMe();
    expect(new Headers(fetch.mock.calls[0][1].headers).get('authorization'))
      .toBe('Bearer current-token');
  });

  test(`SSE token replaces configured ${casing} and preserves custom content type`, async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('data: {"type":"run_finished"}\n\n'));
    vi.stubGlobal('fetch', fetch);
    const client = new SSEClient({
      url: 'https://example.test/stream', token: 'current-token', method: 'POST',
      headers: { [casing]: 'Bearer old-token', 'content-type': 'application/custom+json' },
      onEvent: () => {}, maxRetries: 0,
    });
    await client.connect();
    client.close();
    const headers = new Headers(fetch.mock.calls[0][1].headers);
    expect(headers.get('authorization')).toBe('Bearer current-token');
    expect(headers.get('content-type')).toBe('application/custom+json');
  });
}

test('SSE custom header casing overrides default Accept and Cache-Control once', async () => {
  const fetch = vi.fn().mockResolvedValue(new Response('data: {"type":"run_finished"}\n\n'));
  vi.stubGlobal('fetch', fetch);
  const client = new SSEClient({
    url: 'https://example.test/stream', maxRetries: 0, onEvent: () => {},
    headers: { accept: 'application/custom', 'CACHE-CONTROL': 'private' },
  });
  await client.connect();
  client.close();
  const headers = new Headers(fetch.mock.calls[0][1].headers);
  expect(headers.get('accept')).toBe('application/custom');
  expect(headers.get('cache-control')).toBe('private');
});

for (const headers of [
  { authorization: 'Bearer request-token', 'content-type': 'application/octet-stream' },
  new Headers({ authorization: 'Bearer request-token', 'content-type': 'application/octet-stream' }),
]) {
  test(`public fetch preserves per-request override precedence (${headers.constructor.name})`, async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('{}', { headers: { 'Content-Type': 'application/json' } }));
    vi.stubGlobal('fetch', fetch);
    const client = new AstraClient({
      baseUrl: 'https://example.test', accessToken: 'current-token',
      headers: { AUTHORIZATION: 'Bearer configured-token' },
    });
    await client.fetch('/synthetic', { method: 'POST', body: 'data', headers });
    const sent = new Headers(fetch.mock.calls[0][1].headers);
    expect(sent.get('authorization')).toBe('Bearer request-token');
    expect(sent.get('content-type')).toBe('application/octet-stream');
  });
}

for (const headers of [
  { authorization: 'Bearer override-token' },
  [['AUTHORIZATION', 'Bearer override-token']] as [string, string][],
  new Headers({ authorization: 'Bearer override-token' }),
  { forEach: (callback: (value: string, key: string) => void) =>
    callback('Bearer override-token', 'authorization') } as Headers,
]) {
  test(`request header override replaces all prior casings (${headers.constructor.name})`, () => {
    const merged = headersInitToRecord({
      Authorization: 'Bearer stale-token', AUTHORIZATION: 'Bearer duplicate-token',
      'X-Custom': 'preserved',
    }, headers);
    expect(new Headers(merged).get('authorization')).toBe('Bearer override-token');
    expect(new Headers(merged).get('x-custom')).toBe('preserved');
  });
}
