import { useEffect, useRef } from "react";
import { IconAlertCircle } from "@tabler/icons-react";

export function ExtensionError({
  message,
  draftPreserved = false,
}: {
  message: string;
  draftPreserved?: boolean;
}) {
  const element = useRef<HTMLDivElement>(null);
  useEffect(() => {
    element.current?.scrollIntoView({ block: "nearest" });
  }, [message]);
  return (
    <div
      ref={element}
      role="alert"
      className="rounded-xl border border-danger/20 bg-danger/5 px-3.5 py-3 text-xs leading-relaxed text-danger"
    >
      <div className="flex items-start gap-2">
        <IconAlertCircle size={16} className="mt-0.5 shrink-0" />
        <div className="min-w-0 flex-1 space-y-1">
          <div className="break-words font-medium">{message}</div>
          {draftPreserved && (
            <p className="m-0 text-xs text-muted">
              草稿已保留，处理上述问题后可以继续编辑。
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
