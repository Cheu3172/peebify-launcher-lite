// ------------ Modal Root ------------
// Where dialogs show up. Reads the modal store and draws either a confirm dialog (with an optional red danger
// style) or the install dialog.
import { useId } from "react";
import { AlertTriangle } from "lucide-react";
import { useModalStore } from "../../store/modalStore";
import { Modal } from "./Modal";
import { InstallModalBody } from "./InstallModalBody";
import { MODAL_DANGER, MODAL_PRIMARY, MODAL_SECONDARY } from "./modalButtons";

export function ModalRoot() {
  const modal = useModalStore((s) => s.modal);
  const close = useModalStore((s) => s.close);
  const titleId = useId();
  const messageId = useId();
  const isConfirm = modal?.kind === "confirm";
  const infoOnly = modal?.kind === "confirm" && modal.cancelLabel === null;
  const confirm = () => {
    if (modal?.kind === "confirm") modal.onConfirm();
    close();
  };

  const width = modal?.kind === "install" ? 500 : 420;
  const isForm = modal?.kind === "install";

  return (
    <Modal
      open={!!modal}
      onClose={infoOnly ? confirm : close}
      width={width}
      dismissOnBackdrop={!isForm}
      labelledBy={isConfirm ? titleId : undefined}
      describedBy={isConfirm ? messageId : undefined}
    >
      {modal?.kind === "confirm" && (
        <div className="p-[22px]">
          <div className="mb-[8px] flex items-center gap-[10px]">
            {modal.danger && <AlertTriangle size={18} className="shrink-0 text-red-400" />}
            <h2 id={titleId} className="text-[16px] font-semibold">
              {modal.title}
            </h2>
          </div>
          <p id={messageId} className="mb-[20px] text-[13px] leading-[1.55] text-white/65">
            {modal.message}
          </p>
          <div className="flex justify-end gap-[10px]">
            {!infoOnly && (
              <button onClick={close} className={MODAL_SECONDARY}>
                {modal.cancelLabel ?? "Cancel"}
              </button>
            )}
            <button
              autoFocus={!modal.danger || infoOnly}
              onClick={confirm}
              className={modal.danger ? MODAL_DANGER : MODAL_PRIMARY}
            >
              {modal.confirmLabel ?? "Confirm"}
            </button>
          </div>
        </div>
      )}

      {modal?.kind === "install" && <InstallModalBody gameId={modal.gameId} onClose={close} />}
    </Modal>
  );
}
