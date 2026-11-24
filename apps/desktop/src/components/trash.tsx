import { useState } from "react";
import { api } from "../api";
import { errorMessage } from "../lib/format";
import { Button } from "./ui";

/**
 * Opens ~/.Trash in Finder. There is deliberately no "Empty Trash" action: the Trash may hold
 * unrelated files, so emptying it stays the user's call, in Finder.
 */
export function ShowTrashButton({ size = "sm" }: { size?: "sm" | "md" }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <span className="inline-flex flex-col items-start gap-1">
      <Button
        size={size}
        onClick={async () => {
          try {
            await api.revealTrash();
            setError(null);
          } catch (e) {
            setError(errorMessage(e));
          }
        }}
      >
        Show Trash in Finder
      </Button>
      {error && <span className="text-[11.5px] text-critical-text">{error}</span>}
    </span>
  );
}
