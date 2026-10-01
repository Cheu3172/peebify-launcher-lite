// ------------ Notification Icons ------------
// The icon and colors for each notification type (info, success, warning, error).
import { Info, CheckCircle2, AlertTriangle, XCircle } from "lucide-react";
import type { NotifType } from "../store/notificationStore";

export const NOTIF_ICONS: Record<NotifType, typeof Info> = {
  info: Info,
  success: CheckCircle2,
  warning: AlertTriangle,
  error: XCircle,
};

export const NOTIF_ICON_COLORS: Record<NotifType, { bg: string; fg: string }> = {
  info: { bg: "rgba(var(--accent-a-rgb), .14)", fg: "var(--accent-text)" },
  success: { bg: "rgba(52,211,153,.18)", fg: "#6ee7b7" },
  warning: { bg: "rgba(250,204,21,.18)", fg: "#fcd34d" },
  error: { bg: "rgba(239,68,68,.18)", fg: "#fca5a5" },
};
