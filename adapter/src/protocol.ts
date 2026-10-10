export type Entry = { kind: 'file' | 'folder'; uid: string; version: string; size: number; hash: string | null };
export function relativePath(value: unknown): string {
    if (typeof value !== 'string' || !value || value.startsWith('/') || value.includes('\0') ||
        value.split('/').some(p => !p || p === '.' || p === '..' || p.startsWith('.peedrive'))) {
        throw new Error('Invalid relative path');
    }
    return value;
}
export function sameEntry(a: Entry | null, b: Entry | null): boolean {
    return a === null || b === null ? a === b : a.uid === b.uid && a.version === b.version && a.kind === b.kind;
}
export function requireExpected(actual: Entry | null, expected: Entry | null) {
    if (!sameEntry(actual, expected)) throw new Error('Remote item changed since planning. Review and retry.');
}
export function requireString(value: unknown): string {
    if (typeof value !== 'string' || !value || value.includes('\0')) throw new Error('Missing string argument');
    return value;
}
