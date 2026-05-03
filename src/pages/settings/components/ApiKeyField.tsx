import type { KeyState } from "../types";
import { inputClass } from "../types";
import { Btn } from "./Btn";

export function ApiKeyField({
  state,
  setState,
  placeholder,
  onSave,
  onTest,
}: {
  state: KeyState;
  setState: React.Dispatch<React.SetStateAction<KeyState>>;
  placeholder: string;
  onSave: () => void;
  onTest: () => void;
}) {
  return (
    <>
      <div className="flex gap-2">
        <input
          type="password"
          value={state.draft}
          onChange={(e) => setState((p) => ({ ...p, draft: e.target.value }))}
          placeholder={state.hasKey ? "•••••••• stored" : placeholder}
          className={inputClass + " flex-1"}
        />
        <Btn onClick={onSave} disabled={!state.draft.trim()}>
          Save
        </Btn>
        <Btn onClick={onTest} disabled={!state.hasKey || state.testing}>
          {state.testing ? "Testing…" : "Test"}
        </Btn>
      </div>
      {state.result?.ok === true && (
        <p className="text-sm text-green-600 dark:text-green-400 mt-2">
          Connected ✓
        </p>
      )}
      {state.result?.ok === false && (
        <p className="text-sm text-red-600 dark:text-red-400 mt-2 break-all">
          {state.result.message}
        </p>
      )}
    </>
  );
}
