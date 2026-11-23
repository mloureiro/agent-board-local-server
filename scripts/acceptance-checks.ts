import { createServer } from 'node:net';

export async function freePort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  await new Promise((done) => server.close(done));
  if (address === null || typeof address === 'string') throw new Error('No free port');
  return address.port;
}

export interface ProcessRow {
  pid: number;
  ppid: number;
  command: string;
}

export function parseProcessRows(psOutput: string): ProcessRow[] {
  return psOutput.split('\n').flatMap((line) => {
    const match = /^\s*(\d+)\s+(\d+)\s+(.*)$/.exec(line);
    return match === null ? [] : [{ pid: Number(match[1]), ppid: Number(match[2]), command: match[3] ?? '' }];
  });
}

export function descendantsOf(rows: ProcessRow[], rootPid: number): number[] {
  const found = new Set<number>([rootPid]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const row of rows)
      if (found.has(row.ppid) && !found.has(row.pid)) {
        found.add(row.pid);
        grew = true;
      }
  }
  return [...found].sort((a, b) => a - b);
}

export function survivors(rows: ProcessRow[], needles: string[]): ProcessRow[] {
  return rows.filter((row) => needles.some((needle) => row.command.includes(needle)));
}

export function listenAddresses(lsofOutput: string): string[] {
  return lsofOutput.split('\n').flatMap((line) => {
    const match = /\b(?:TCP|UDP) (\S+)/.exec(line);
    return match?.[1] === undefined ? [] : [match[1]];
  });
}

export const isLoopbackAddress = (address: string): boolean =>
  address.startsWith('127.0.0.1:') || address.startsWith('[::1]:');

export function markerEncodings(marker: string): Buffer[] {
  const utf16le = Buffer.from(marker, 'utf16le');
  const utf16be = Buffer.from(utf16le).swap16();
  return [Buffer.from(marker, 'utf8'), utf16le, utf16be];
}

export const containsMarker = (bytes: Buffer, marker: string): boolean =>
  markerEncodings(marker).some((encoded) => bytes.includes(encoded));

export function isInside(path: string, directory: string): boolean {
  const normal = directory.endsWith('/') ? directory : `${directory}/`;
  return path === directory || path.startsWith(normal);
}

export function runsAppExecutable(command: string, executable: string): boolean {
  const needles = [`.app/Contents/MacOS/${executable}`, `/target/debug/${executable}`, `/target/release/${executable}`];
  return needles.some((needle) => {
    for (let at = command.indexOf(needle); at !== -1; at = command.indexOf(needle, at + 1)) {
      const next = command.charAt(at + needle.length);
      if (next === '' || /\s/.test(next)) return true;
    }
    return false;
  });
}

export function signerOf(codesignOutput: string): string | null {
  if (/^Signature=adhoc$/m.test(codesignOutput)) return 'adhoc';
  return /^Authority=(.+)$/m.exec(codesignOutput)?.[1]?.trim() ?? null;
}

export function sameSigner(signers: Array<string | null>): boolean {
  const [first] = signers;
  return first !== null && first !== undefined && signers.every((signer) => signer === first);
}
