import { describe, expect, it } from 'vitest';
import {
  containsMarker,
  descendantsOf,
  isInside,
  isLoopbackAddress,
  listenAddresses,
  parseProcessRows,
  runsAppExecutable,
  sameSigner,
  signerOf,
  survivors,
} from './acceptance-checks.ts';

describe('the process tree', () => {
  const rows = parseProcessRows(
    [
      '  10     1 /Applications/Agent Board Server.app/Contents/MacOS/agent-board-local-server',
      '  11    10 /Applications/Agent Board Server.app/Contents/MacOS/board-server',
      '  12    11 sh -c something',
      '  20     1 /usr/bin/other',
    ].join('\n'),
  );

  it('takes the app and every descendant, and nothing else', () => {
    expect(descendantsOf(rows, 10)).toEqual([10, 11, 12]);
  });

  it('finds the processes that still run from the bundle or the worker directories', () => {
    expect(survivors(rows, ['Agent Board Server.app/']).map((row) => row.pid)).toEqual([10, 11]);
    expect(survivors(rows, ['/nowhere/worker/'])).toEqual([]);
  });
});

describe('listeners', () => {
  const lsof = [
    'COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME',
    'board-ser 11 u 9u IPv4 0x1 0t0 TCP 127.0.0.1:4785 (LISTEN)',
    'board-ser 11 u 10u IPv6 0x2 0t0 TCP [::1]:4786 (LISTEN)',
    'board-ser 11 u 11u IPv4 0x3 0t0 TCP *:4787 (LISTEN)',
    'board-ser 11 u 12u IPv4 0x4 0t0 UDP 0.0.0.0:5353',
  ].join('\n');

  it('reads TCP and UDP addresses and tells loopback from the rest', () => {
    const addresses = listenAddresses(lsof);

    expect(addresses).toEqual(['127.0.0.1:4785', '[::1]:4786', '*:4787', '0.0.0.0:5353']);
    expect(addresses.filter((address) => !isLoopbackAddress(address))).toEqual(['*:4787', '0.0.0.0:5353']);
  });
});

describe('the marker search', () => {
  const marker = 'MARK-9f3a1c';

  it.each([
    ['utf-8', Buffer.from(`xx ${marker} yy`, 'utf8')],
    ['utf-16 little-endian', Buffer.concat([Buffer.from([1, 2, 3]), Buffer.from(marker, 'utf16le')])],
    ['utf-16 big-endian', Buffer.from(marker, 'utf16le').swap16()],
  ])('finds it as %s', (_name, bytes) => {
    expect(containsMarker(bytes, marker)).toBe(true);
  });

  it('does not find it in unrelated bytes or in a half of it', () => {
    expect(containsMarker(Buffer.from('nothing here'), marker)).toBe(false);
    expect(containsMarker(Buffer.from(marker.slice(0, 6)), marker)).toBe(false);
  });
});

describe('isInside', () => {
  it('matches the directory and its children but not a sibling with the same prefix', () => {
    expect(isInside('/a/worker/bin', '/a/worker')).toBe(true);
    expect(isInside('/a/worker', '/a/worker')).toBe(true);
    expect(isInside('/a/worker-two', '/a/worker')).toBe(false);
  });
});

describe('runsAppExecutable()', () => {
  const executable = 'agent-board-local-server';
  const cases: [string, string, boolean][] = [
    ['an installed app', '/Applications/Agent Board Server.app/Contents/MacOS/agent-board-local-server', true],
    [
      'a dev bundle with launch arguments',
      '/Users/me/repo/src-tauri/target/release/bundle/macos/Agent Board Server.app/Contents/MacOS/agent-board-local-server -psn_0_123',
      true,
    ],
    [
      'the raw debug binary run by the dev server',
      '/Users/me/repo/src-tauri/target/debug/agent-board-local-server',
      true,
    ],
    [
      'the raw release binary with arguments',
      '/Users/me/repo/src-tauri/target/release/agent-board-local-server --flag',
      true,
    ],
    [
      'a longer name in the cargo output',
      '/Users/me/repo/src-tauri/target/debug/agent-board-local-server-helper',
      false,
    ],
    [
      'an editor opened on the repository',
      '/Applications/Visual Studio Code.app/Contents/MacOS/Electron /Users/me/workspace/agent-board-local-server',
      false,
    ],
    [
      'the acceptance script itself',
      'node scripts/acceptance.ts --app /Users/me/workspace/agent-board-local-server/src-tauri/target/release/bundle/macos/Agent Board Server.app',
      false,
    ],
    ['a sidecar of the same bundle', '/Applications/Agent Board Server.app/Contents/MacOS/board-server', false],
    ['a longer executable name', '/Applications/X.app/Contents/MacOS/agent-board-local-server-helper', false],
  ];

  it.each(cases)('%s', (_name, command, expected) => {
    expect(runsAppExecutable(command, executable)).toBe(expected);
  });
});

describe('the signer of a bundle part', () => {
  const adhoc = ['Executable=/x/board', 'Identifier=board', 'Signature=adhoc', 'TeamIdentifier=not set'].join('\n');
  const named = [
    'Executable=/x/board',
    'Signature size=1800',
    'Authority=Agent Board Local Signing',
    'TeamIdentifier=not set',
  ].join('\n');

  it('reads adhoc and the authority name', () => {
    expect(signerOf(adhoc)).toBe('adhoc');
    expect(signerOf(named)).toBe('Agent Board Local Signing');
  });

  it('reads no signer from an unsigned part', () => {
    expect(signerOf('Executable=/x/board\ncode object is not signed at all')).toBeNull();
  });

  it('accepts three equal signers and refuses a mixed or missing one', () => {
    expect(sameSigner(['adhoc', 'adhoc', 'adhoc'])).toBe(true);
    expect(sameSigner(['Name', 'Name', 'Name'])).toBe(true);
    expect(sameSigner(['Name', 'adhoc', 'Name'])).toBe(false);
    expect(sameSigner(['adhoc', null, 'adhoc'])).toBe(false);
    expect(sameSigner([])).toBe(false);
  });
});
