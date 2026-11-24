// Confirm sheet: dry-run plan → Trash (default) or Delete (second confirmation) → live per-item
// progress from `clean://item` → result (freed now / moved to Trash / failed).

import * as Dialog from "@radix-ui/react-dialog";
import { type ReactNode, useEffect, useState } from "react";
import { api } from "../api";
import type { CleanItemResult } from "../bindings/CleanItemResult";
import type { CleanMode } from "../bindings/CleanMode";
import type { CleanPlan } from "../bindings/CleanPlan";
import type { CleanResult } from "../bindings/CleanResult";
import type { Finding } from "../bindings/Finding";
import { CheckCircleIcon, InfoIcon, MinusCircleIcon, TrashIcon, WarningIcon, XCircleIcon } from "../components/icons";
import { ShowTrashButton } from "../components/trash";
import { Button, Callout, cx, RiskBadge, Spinner } from "../components/ui";
import { actionDescription, doneDescription, isPermanentAction, isSelectable } from "../lib/findings";
import { errorMessage, formatBytes, formatCount, isNotImplemented, stubDetail, tildify } from "../lib/format";
import { useApp } from "../state";

type Phase =
  | { kind: "planning" }
  | { kind: "plan-error"; message: string }
  | { kind: "ready"; plan: CleanPlan }
  | { kind: "confirm-delete"; plan: CleanPlan }
  | { kind: "executing"; plan: CleanPlan }
  | { kind: "done"; plan: CleanPlan; result: CleanResult }
  | { kind: "exec-error"; plan: CleanPlan; message: string };

export function ConfirmDialog() {
  const { confirmOpen, setConfirmOpen } = useApp();
  return (
    <Dialog.Root open={confirmOpen} onOpenChange={setConfirmOpen}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 animate-fade-in bg-black/25 dark:bg-black/45" />
        {confirmOpen && <ConfirmContent onClose={() => setConfirmOpen(false)} />}
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function ConfirmContent({ onClose }: { onClose: () => void }) {
  const { report, selection, applyCleanResult } = useApp();
  // Freeze the request when the sheet opens; selection changes underneath must not alter it.
  const [ids] = useState(() =>
    (report?.findings ?? []).filter((f) => selection.has(f.id) && isSelectable(f)).map((f) => f.id),
  );
  const [mode, setMode] = useState<CleanMode>("trash");
  const [phase, setPhase] = useState<Phase>({ kind: "planning" });
  const [items, setItems] = useState<ReadonlyMap<string, CleanItemResult>>(new Map());

  useEffect(() => {
    let cancelled = false;
    api.planClean({ findingIds: ids, mode: "trash", dryRun: true }).then(
      (plan) => !cancelled && setPhase({ kind: "ready", plan }),
      (e) => !cancelled && setPhase({ kind: "plan-error", message: errorMessage(e) }),
    );
    return () => {
      cancelled = true;
    };
  }, [ids]);

  const busy = phase.kind === "executing" || phase.kind === "planning";

  async function execute(plan: CleanPlan) {
    setPhase({ kind: "executing", plan });
    setItems(new Map());
    const stop = await api.onCleanItem((item) => setItems((prev) => new Map(prev).set(item.findingId, item)));
    try {
      const result = await api.executeClean({ findingIds: plan.items.map((f) => f.id), mode, dryRun: false });
      setItems(new Map(result.items.map((i) => [i.findingId, i])));
      setPhase({ kind: "done", plan, result });
      await applyCleanResult(result);
    } catch (e) {
      setPhase({ kind: "exec-error", plan, message: errorMessage(e) });
    } finally {
      stop();
    }
  }

  const plan = "plan" in phase ? phase.plan : null;
  const choosing = phase.kind === "ready" || phase.kind === "confirm-delete";
  const title =
    phase.kind === "done"
      ? "Clean finished"
      : phase.kind === "executing"
        ? "Cleaning…"
        : `Clean ${formatCount(ids.length, "item")}?`;

  return (
    <Dialog.Content
      onEscapeKeyDown={(e) => busy && e.preventDefault()}
      onPointerDownOutside={(e) => e.preventDefault()}
      onInteractOutside={(e) => e.preventDefault()}
      className="fixed top-1/2 left-1/2 z-50 flex max-h-[calc(100vh-72px)] w-[min(640px,calc(100vw-48px))] -translate-x-1/2 -translate-y-1/2 animate-dialog-in flex-col overflow-hidden rounded-xl border border-line-strong bg-surface shadow-2xl"
    >
      <div className="shrink-0 px-5 pt-4 pb-3">
        <Dialog.Title className="text-[15px] font-semibold">{title}</Dialog.Title>
        <Dialog.Description className="mt-0.5 text-[12.5px] text-fg-2">
          {phase.kind === "done"
            ? "Here is what happened to each item."
            : phase.kind === "executing"
              ? "Each item is checked again right before it is removed."
              : "Nothing is removed until you confirm. Each item is checked again right before it is removed."}
        </Dialog.Description>
      </div>

      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-5 pb-4">
        {phase.kind === "planning" && (
          <div className="flex items-center gap-2 py-8 text-[12.5px] text-fg-2">
            <Spinner /> Checking the selected items…
          </div>
        )}
        {phase.kind === "plan-error" && (
          <Callout tone="critical" title="Can't prepare this clean">
            {isNotImplemented(phase.message)
              ? `Cleaning isn't available in this build yet (${stubDetail(phase.message)}). Nothing was changed.`
              : phase.message}
          </Callout>
        )}
        {phase.kind === "done" && <ResultSummary result={phase.result} />}
        {phase.kind === "exec-error" && (
          <Callout tone="critical" title="The clean stopped">
            {phase.message}. Items not listed as done were left in place.
          </Callout>
        )}
        {plan && phase.kind !== "done" && <PlanNotes plan={plan} />}
        {plan && <PlanItems plan={plan} mode={mode} results={items} showStatus={!choosing} />}
      </div>

      {plan && phase.kind === "ready" && (
        <div className="shrink-0 border-t border-line px-5 pt-3 pb-3">
          <ModeChoice mode={mode} setMode={setMode} />
        </div>
      )}

      <Footer
        phase={phase}
        mode={mode}
        onClose={onClose}
        onExecute={execute}
        onConfirmDelete={(p) => setPhase({ kind: "confirm-delete", plan: p })}
        onBack={(p) => setPhase({ kind: "ready", plan: p })}
      />
    </Dialog.Content>
  );
}

function PlanItems({
  plan,
  mode,
  results,
  showStatus,
}: {
  plan: CleanPlan;
  mode: CleanMode;
  results: ReadonlyMap<string, CleanItemResult>;
  showStatus: boolean;
}) {
  return (
    <ul className="divide-y divide-line overflow-hidden rounded-lg border border-line">
      {plan.items.map((f) => (
        <PlanItem key={f.id} finding={f} mode={mode} result={results.get(f.id)} showStatus={showStatus} />
      ))}
    </ul>
  );
}

function PlanItem({
  finding: f,
  mode,
  result,
  showStatus,
}: {
  finding: Finding;
  mode: CleanMode;
  result: CleanItemResult | undefined;
  showStatus: boolean;
}) {
  return (
    <li className="flex items-start gap-2.5 px-3 py-2">
      {showStatus && (
        <span className="mt-px shrink-0">
          {!result ? (
            <Spinner size={15} />
          ) : result.outcome === "skipped" ? (
            <MinusCircleIcon className="text-fg-3" aria-label="Skipped" />
          ) : result.outcome === "done" ? (
            <CheckCircleIcon className="text-good" aria-label="Done" />
          ) : (
            <XCircleIcon className="text-critical" aria-label="Failed" />
          )}
        </span>
      )}
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-1.5">
          <span className="truncate text-[12.5px] font-medium">{f.label}</span>
          <RiskBadge risk={f.risk} />
        </div>
        <div className="selectable truncate font-mono text-[10.5px] text-fg-3" title={f.path ?? undefined}>
          {f.path ? tildify(f.path) : (f.command ?? []).join(" ")}
        </div>
        {result?.outcome === "done" && !result.message ? (
          <div className="mt-0.5 text-[11.5px] text-good-text">{doneDescription(f, mode)}</div>
        ) : result?.message ? (
          <div
            className={cx(
              "selectable mt-0.5 text-[11.5px]",
              result.outcome === "failed" ? "text-critical-text" : "text-fg-2",
            )}
          >
            {result.message}
          </div>
        ) : (
          <div className={cx("mt-0.5 text-[11.5px]", isPermanentAction(f) ? "text-warn-text" : "text-fg-2")}>
            {actionDescription(f, mode)}
          </div>
        )}
      </div>
      <span className="shrink-0 text-[12.5px] font-medium tabular-nums">{formatBytes(f.bytes)}</span>
    </li>
  );
}

/** Core warnings plus the note that command items (docker, xcrun, git worktree prune) are permanent. */
function PlanNotes({ plan }: { plan: CleanPlan }) {
  const permanent = plan.items.filter(isPermanentAction);
  const one = permanent.length === 1;
  const notes = [
    ...plan.warnings,
    ...(permanent.length > 0
      ? [
          `${formatCount(permanent.length, "item")} ${one ? "runs its" : "run their"} own tool (docker, xcrun, git worktree prune) and ${one ? "frees" : "free"} space right away, whichever option you pick.`,
        ]
      : []),
  ];
  if (notes.length === 0) return null;
  return (
    <Callout tone="warn" title="Check before you continue">
      <ul className="list-disc space-y-0.5 pl-4">
        {notes.map((w) => (
          <li key={w}>{w}</li>
        ))}
      </ul>
    </Callout>
  );
}

function ModeChoice({ mode, setMode }: { mode: CleanMode; setMode: (m: CleanMode) => void }) {
  const options: { value: CleanMode; title: string; body: string }[] = [
    { value: "trash", title: "Move to Trash", body: "Recoverable. Frees no space until you empty the Trash." },
    { value: "delete", title: "Delete permanently", body: "Frees the space right away. Can't be undone." },
  ];
  return (
    <fieldset className="space-y-2">
      <legend className="sr-only">How to remove</legend>
      <div className="grid grid-cols-2 gap-2">
        {options.map((o) => (
          <label
            key={o.value}
            className={cx(
              "flex cursor-default gap-2 rounded-lg border px-2.5 py-2",
              mode === o.value
                ? o.value === "delete"
                  ? "border-critical bg-critical-soft"
                  : "border-accent bg-accent-soft"
                : "border-line hover:bg-surface-2",
            )}
          >
            <input
              type="radio"
              name="clean-mode"
              value={o.value}
              checked={mode === o.value}
              onChange={() => setMode(o.value)}
              className="mt-0.5"
            />
            <span>
              <span className="block text-[12.5px] font-medium">{o.title}</span>
              <span className="block text-[11.5px] leading-snug text-fg-2">{o.body}</span>
            </span>
          </label>
        ))}
      </div>
      {mode === "trash" && (
        <p className="flex items-center gap-1.5 text-[11.5px] text-fg-2">
          <InfoIcon size={13} className="shrink-0 text-accent" />
          <span>
            <strong className="font-semibold text-fg">Trash frees nothing until it is emptied</strong> — the files stay
            on this disk until you empty the Trash in Finder.
          </span>
        </p>
      )}
    </fieldset>
  );
}

function ResultSummary({ result }: { result: CleanResult }) {
  const failed = result.items.filter((i) => i.outcome !== "done");
  if (result.items.length === 0) {
    return (
      <Callout tone="warn" title="Nothing was cleaned">
        The clean engine returned no results — cleaning isn't available in this build yet. Nothing was changed.
      </Callout>
    );
  }
  return (
    <div className="space-y-3">
      <div className="grid grid-cols-3 gap-2">
        <Stat label="Freed now" value={formatBytes(result.freedBytes)} />
        <Stat label="Moved to Trash" value={formatBytes(result.movedToTrashBytes)} />
        <Stat
          label="Failed or skipped"
          value={String(failed.length)}
          icon={failed.length > 0 ? <WarningIcon size={14} className="text-warn" /> : undefined}
        />
      </div>
      {result.movedToTrashBytes > 0 && (
        <Callout
          tone="info"
          title={`${formatBytes(result.movedToTrashBytes)} is in the Trash`}
          action={<ShowTrashButton />}
        >
          It still takes up disk space until you empty the Trash in Finder.
        </Callout>
      )}
    </div>
  );
}

function Stat({ label, value, icon }: { label: string; value: string; icon?: ReactNode }) {
  return (
    <div className="rounded-lg bg-surface-2 px-3 py-2">
      <div className="text-[11.5px] text-fg-2">{label}</div>
      <div className="flex items-center gap-1.5 text-[18px] font-semibold">
        {icon}
        {value}
      </div>
    </div>
  );
}

function Footer({
  phase,
  mode,
  onClose,
  onExecute,
  onConfirmDelete,
  onBack,
}: {
  phase: Phase;
  mode: CleanMode;
  onClose: () => void;
  onExecute: (plan: CleanPlan) => void;
  onConfirmDelete: (plan: CleanPlan) => void;
  onBack: (plan: CleanPlan) => void;
}) {
  const plan = "plan" in phase ? phase.plan : null;
  const total = plan?.totalBytes ?? 0;

  if (phase.kind === "confirm-delete") {
    return (
      <div className="shrink-0 space-y-3 border-t border-line bg-critical-soft px-5 py-3">
        <div className="flex items-start gap-2.5">
          <WarningIcon className="mt-0.5 shrink-0 text-critical" size={18} />
          <div className="text-[12.5px]">
            <div className="font-semibold">
              Permanently delete {formatCount(phase.plan.items.length, "item")} ({formatBytes(total)})?
            </div>
            <div className="text-fg-2">They skip the Trash and can't be recovered.</div>
          </div>
        </div>
        <div className="flex justify-end gap-2">
          <Button onClick={() => onBack(phase.plan)}>Back</Button>
          <Button variant="destructive" onClick={() => onExecute(phase.plan)} autoFocus>
            Delete Permanently
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="flex shrink-0 items-center gap-2 border-t border-line bg-surface-2/60 px-5 py-3">
      {plan && phase.kind !== "done" && (
        <div className="text-[12.5px]">
          <span className="text-fg-2">Total </span>
          <span className="font-semibold tabular-nums">{formatBytes(total)}</span>
        </div>
      )}
      <span className="flex-1" />
      {phase.kind === "done" || phase.kind === "exec-error" || phase.kind === "plan-error" ? (
        <Button variant="primary" onClick={onClose} autoFocus>
          Done
        </Button>
      ) : (
        <>
          <Button onClick={onClose} disabled={phase.kind === "executing"}>
            Cancel
          </Button>
          {mode === "trash" ? (
            <Button
              variant="primary"
              disabled={phase.kind !== "ready" || phase.plan.items.length === 0}
              onClick={() => plan && onExecute(plan)}
            >
              <TrashIcon size={14} />
              Move to Trash
            </Button>
          ) : (
            <Button
              variant="destructive"
              disabled={phase.kind !== "ready" || phase.plan.items.length === 0}
              onClick={() => plan && onConfirmDelete(plan)}
            >
              Delete…
            </Button>
          )}
        </>
      )}
    </div>
  );
}
