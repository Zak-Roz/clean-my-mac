// Storage-view state kept outside the React tree (ARCHITECTURE.md §11): a measurement takes tens
// of seconds and keeps running, with its result kept, while the user is on another screen.
// The screen reads it with `useStorage()`; everything that talks to the backend goes through `api`.

import { useSyncExternalStore } from "react";
import { api } from "./api";
import type { StorageBreakdown } from "./bindings/StorageBreakdown";
import type { StorageNode } from "./bindings/StorageNode";
import type { StorageProgress } from "./bindings/StorageProgress";
import { errorMessage } from "./lib/format";
import { isFolderRest, type Loaded, ROOT_ID } from "./lib/storage";

export interface StorageSnapshot {
  breakdown: StorageBreakdown | null;
  /** The stored breakdown was asked for at least once. */
  loaded: boolean;
  measuring: boolean;
  progress: StorageProgress | null;
  error: string | null;
  /** Live folder listings, by the id of the folder they list. */
  listings: Loaded;
  /** Folder ids whose listing is loading. */
  listing: ReadonlySet<string>;
  listingError: string | null;
  /** Ids from the top to the level on screen (kept while the user is elsewhere). */
  trail: readonly string[];
}

let snapshot: StorageSnapshot = {
  breakdown: null,
  loaded: false,
  measuring: false,
  progress: null,
  error: null,
  listings: new Map(),
  listing: new Set(),
  listingError: null,
  trail: [ROOT_ID],
};
const listeners = new Set<() => void>();

function set(patch: Partial<StorageSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  for (const listener of listeners) listener();
}

/** The folder a row lists when opened: itself, or for a folder's "smaller items" the folder. */
function listedFolder(node: StorageNode): { id: string; path: string } | null {
  if (node.path === null) return null;
  if (isFolderRest(node)) return { id: node.path, path: node.path };
  if (node.kind === "folder") return { id: node.id, path: node.path };
  return null;
}

export const storage = {
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },

  get(): StorageSnapshot {
    return snapshot;
  },

  /** The stored breakdown with found marks for the current scan (cheap; no walk). */
  async load(): Promise<void> {
    try {
      const breakdown = await api.storageCached();
      // A measurement that finished meanwhile is newer than what was stored before it.
      if (!snapshot.measuring) set({ breakdown, error: null });
    } catch (e) {
      set({ error: errorMessage(e) });
    } finally {
      set({ loaded: true });
    }
  },

  /** Walks the disk again. One at a time; progress arrives as events. */
  async measure(): Promise<void> {
    if (snapshot.measuring) return;
    set({ measuring: true, progress: null, error: null });
    const unlisten = api.onStorageProgress((progress) => set({ progress }));
    try {
      const breakdown = await api.measureStorage();
      set({ breakdown, listings: new Map(), listingError: null });
    } catch (e) {
      set({ error: errorMessage(e) });
    } finally {
      (await unlisten)();
      set({ measuring: false, progress: null, loaded: true });
    }
  },

  /** Lists a folder the breakdown did not keep that deep (or all of a folder's smaller items). */
  async list(node: StorageNode): Promise<void> {
    const folder = listedFolder(node);
    if (!folder || snapshot.listing.has(folder.id)) return;
    set({ listing: new Set([...snapshot.listing, folder.id]), listingError: null });
    try {
      const listed = await api.storageChildren(folder.path);
      set({ listings: new Map([...snapshot.listings, [folder.id, listed]]) });
    } catch (e) {
      set({ listingError: errorMessage(e) });
    } finally {
      const listing = new Set(snapshot.listing);
      listing.delete(folder.id);
      set({ listing });
    }
  },

  setTrail(trail: readonly string[]): void {
    set({ trail });
  },
};

export function useStorage(): StorageSnapshot {
  return useSyncExternalStore(storage.subscribe, storage.get);
}
