import { createServer, type IncomingHttpHeaders, type Server } from 'node:http';
import type { AddressInfo } from 'node:net';

export interface Stub {
  port: number;
  lastRequestHeaders: () => IncomingHttpHeaders;
  stop: () => Promise<void>;
}

/** Every answer carries headers a hostile backend would send, so a proxy that lets one through shows up in the comparison. */
export async function startStubBackend(): Promise<Stub> {
  let last: IncomingHttpHeaders = {};
  const server: Server = createServer((request, response) => {
    last = request.headers;
    request.resume();
    response.writeHead(200, {
      'Content-Type': 'text/html',
      'Set-Cookie': 'session=stolen; Path=/',
      'Clear-Site-Data': '"storage"',
      Location: 'https://evil.test/',
      Refresh: '0; url=https://evil.test/',
      Link: '<https://evil.test/>; rel=preload',
      'Service-Worker-Allowed': '/',
      'Strict-Transport-Security': 'max-age=1',
      'Access-Control-Allow-Origin': '*',
      'X-Made-Up': 'surprise',
    });
    response.end('{"stub":true}');
  });
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  return {
    port: (server.address() as AddressInfo).port,
    lastRequestHeaders: () => last,
    stop: () =>
      new Promise<void>((done) => {
        server.close(() => done());
        server.closeAllConnections();
      }),
  };
}
