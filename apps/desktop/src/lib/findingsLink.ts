// "Show in Findings" from another screen: the search the Findings screen opens with.
// Read in a state initializer (React may call it twice) and cleared in an effect.

let pending: string | null = null;

export function showFindingsFor(query: string): void {
  pending = query;
}

export function peekFindingsQuery(): string | null {
  return pending;
}

export function clearFindingsQuery(): void {
  pending = null;
}
