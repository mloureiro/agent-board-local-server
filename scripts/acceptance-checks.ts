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

export const ROOT_SECRET_ACCOUNTS = [
  'proxy-secret',
  'worker-token',
  'worker-data-key',
  'worker-token-pending',
  'worker-data-key-pending',
] as const;
