import { LuCheck, LuInfo, LuTriangleAlert, LuX } from "react-icons/lu";
import { useNotificationStore } from "../../store/notifications";

export function NotificationContainer() {
  const notifications = useNotificationStore((state) => state.notifications);
  const removeNotification = useNotificationStore((state) => state.removeNotification);

  const getNotificationStyles = (type: string) => {
    switch (type) {
      case "success":
        return "app-toast app-toast-success";
      case "error":
        return "app-toast app-toast-error";
      case "warning":
        return "app-toast app-toast-warning";
      case "info":
      default:
        return "app-toast app-toast-info";
    }
  };

  const getNotificationIcon = (type: string) => {
    switch (type) {
      case "success":
        return <LuCheck className="app-icon app-text-success" />;
      case "error":
        return <LuX className="app-icon app-text-danger" />;
      case "warning":
        return <LuTriangleAlert className="app-icon app-text-warning" />;
      case "info":
      default:
        return <LuInfo className="app-icon app-text-accent" />;
    }
  };

  return (
    <div className="fixed top-4 right-4 z-50 space-y-2 max-w-md">
      {notifications.map((notification) => (
        <div
          key={notification.id}
          role={notification.type === "error" ? "alert" : "status"}
          className={`${getNotificationStyles(notification.type)} p-4 rounded-lg animate-slide-in`}
        >
          <div className="flex items-start justify-between">
            <div className="flex items-start gap-3">
              <span className="mt-0.5 inline-flex">{getNotificationIcon(notification.type)}</span>
              <p className="text-sm">{notification.message}</p>
            </div>
            <button
              onClick={() => removeNotification(notification.id)}
              className="app-text-muted hover:opacity-80 ml-4"
              aria-label="Dismiss notification"
            >
              <LuX className="app-icon" />
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
