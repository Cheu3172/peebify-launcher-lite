// ------------ Delete Capture Button ------------
// The red Delete button for one or many captures. Asks first, then sends the files to the Recycle Bin.
import { useState } from "react";
import { Loader2, Trash2 } from "lucide-react";
import type { MediaItem } from "../../lib/ipc";
import { useMediaStore } from "../../store/mediaStore";
import { useModalStore } from "../../store/modalStore";
import { ActionButton } from "../ui/ActionButton";

export function DeleteButton({
  items,
  onDeleted,
}: {
  items: MediaItem[];
  onDeleted?: () => void;
}) {
  const [pending, setPending] = useState(false);
  const remove = useMediaStore((s) => s.remove);
  const openConfirm = useModalStore((s) => s.openConfirm);

  const confirm = () => {
    const count = items.length;
    if (count === 0) return;
    const noun = `${count} capture${count === 1 ? "" : "s"}`;
    openConfirm({
      title: `Delete ${noun}?`,
      message:
        count === 1
          ? "The file moves to the Recycle Bin, where it can be recovered. If it can't go to the Recycle Bin, it is not deleted."
          : "The files move to the Recycle Bin, where they can be recovered. Any that can't go to the Recycle Bin are not deleted.",
      confirmLabel: "Delete",
      danger: true,
      onConfirm: () => {
        setPending(true);
        void remove(items.map((i) => i.path))
          .then((deleted) => {
            if (deleted > 0) onDeleted?.();
          })
          .finally(() => setPending(false));
      },
    });
  };

  return (
    <ActionButton
      variant="danger"
      disabled={pending || items.length === 0}
      icon={pending ? <Loader2 size={14} className="animate-spin" /> : <Trash2 size={14} />}
      onClick={confirm}
    >
      Delete
    </ActionButton>
  );
}
