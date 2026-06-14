import { ComponentPropsWithoutRef, useEffect } from "react";
import { createPortal } from "react-dom";

export const APP_DIALOG_PORTAL_CHANGE_EVENT = "app-dialog-portal-change";

type AppDialogPortalProps = ComponentPropsWithoutRef<"div">;

export function AppDialogPortal({ children, ...props }: AppDialogPortalProps) {
  useEffect(() => {
    document.dispatchEvent(new Event(APP_DIALOG_PORTAL_CHANGE_EVENT));
    return () => {
      document.dispatchEvent(new Event(APP_DIALOG_PORTAL_CHANGE_EVENT));
    };
  }, []);

  if (typeof document === "undefined") return null;

  return createPortal(<div {...props}>{children}</div>, document.body);
}
