import { memo, useSyncExternalStore } from "react";

import type { Confirmation } from "./App.types";
import { ConfirmationDialog } from "./AppDialogs";
import {
  createExternalStore,
  type ExternalStore,
  useExternalStore,
} from "./externalStore";

export type ConfirmationController = ExternalStore<Confirmation | null>;

export function useConfirmationController(): ConfirmationController {
  return useExternalStore(() => createExternalStore<Confirmation | null>(null));
}

type ConfirmationDialogHostProps = {
  container: HTMLElement | null;
  controller: ConfirmationController;
};

export const ConfirmationDialogHost = memo(function ConfirmationDialogHost({
  container,
  controller,
}: ConfirmationDialogHostProps) {
  const confirmation = useSyncExternalStore(
    controller.subscribe,
    controller.getSnapshot,
    controller.getSnapshot,
  );
  return (
    <ConfirmationDialog
      confirmation={confirmation}
      container={container}
      onClose={() => controller.set(null)}
      onConfirm={(pending) => {
        controller.set(null);
        pending.run();
      }}
    />
  );
});
