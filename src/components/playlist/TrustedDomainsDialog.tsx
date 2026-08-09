import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SyncplayConfig } from "../../types/config";
import { useNotificationStore } from "../../store/notifications";
import { AppDialogPortal } from "../common/AppDialogPortal";

interface TrustedDomainsDialogProps {
  isOpen: boolean;
  onClose: () => void;
}

export function TrustedDomainsDialog({ isOpen, onClose }: TrustedDomainsDialogProps) {
  const [config, setConfig] = useState<SyncplayConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [trustedDomainInput, setTrustedDomainInput] = useState("");
  const addNotification = useNotificationStore((state) => state.addNotification);

  useEffect(() => {
    if (!isOpen) {
      setConfig(null);
      setTrustedDomainInput("");
      return;
    }

    const loadConfig = async () => {
      setLoading(true);
      try {
        const loaded = await invoke<SyncplayConfig>("get_config");
        setConfig(loaded);
      } catch (error) {
        addNotification({
          type: "error",
          message: "Failed to load trusted domain settings",
        });
      } finally {
        setLoading(false);
      }
    };

    loadConfig();
  }, [isOpen, addNotification]);

  const saveConfig = async (nextConfig: SyncplayConfig) => {
    try {
      await invoke("update_config", { config: nextConfig });
      setConfig(nextConfig);
    } catch (error) {
      addNotification({
        type: "error",
        message: "Failed to update trusted domains",
      });
    }
  };

  const addTrustedDomain = async () => {
    if (!config) return;
    const trimmed = trustedDomainInput.trim();
    if (!trimmed) return;
    if (config.user.trusted_domains.includes(trimmed)) {
      addNotification({
        type: "warning",
        message: "Trusted domain already exists",
      });
      setTrustedDomainInput("");
      return;
    }
    const nextConfig: SyncplayConfig = {
      ...config,
      user: {
        ...config.user,
        trusted_domains: [...config.user.trusted_domains, trimmed],
      },
    };
    await saveConfig(nextConfig);
    setTrustedDomainInput("");
  };

  const removeTrustedDomain = async (domain: string) => {
    if (!config) return;
    const nextConfig: SyncplayConfig = {
      ...config,
      user: {
        ...config.user,
        trusted_domains: config.user.trusted_domains.filter((entry) => entry !== domain),
      },
    };
    await saveConfig(nextConfig);
  };

  const toggleOnlySwitch = async (value: boolean) => {
    if (!config) return;
    const nextConfig: SyncplayConfig = {
      ...config,
      user: {
        ...config.user,
        only_switch_to_trusted_domains: value,
      },
    };
    await saveConfig(nextConfig);
  };

  if (!isOpen) return null;

  return (
    <AppDialogPortal className="fixed inset-0 app-overlay app-dialog-overlay flex items-center justify-center">
      <div className="app-panel app-panel-glass rounded-xl p-6 w-full max-w-2xl max-h-[80vh] overflow-auto app-dialog-panel">
        <div className="flex flex-wrap items-center justify-between gap-4 mb-4">
          <div>
            <h2 className="text-xl font-bold">Trusted Domains</h2>
            <p className="text-xs app-text-muted">Control which URLs can be opened.</p>
          </div>
          <button onClick={onClose} className="btn-neutral px-3 py-2 rounded-md text-sm">
            Close
          </button>
        </div>

        {loading && !config ? (
          <div className="text-center py-8">
            <p className="app-text-muted">Loading trusted domains...</p>
          </div>
        ) : config ? (
          <div className="space-y-4">
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={config.user.only_switch_to_trusted_domains}
                onChange={(e) => toggleOnlySwitch(e.target.checked)}
                className="app-checkbox"
              />
              Only switch to trusted domains
            </label>

            <div>
              <label className="block text-sm font-medium mb-1">Trusted domains</label>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={trustedDomainInput}
                  onChange={(e) => setTrustedDomainInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      addTrustedDomain();
                    }
                  }}
                  className="flex-1 app-input px-3 py-2 rounded focus:outline-none"
                  placeholder="youtube.com"
                />
                <button
                  type="button"
                  onClick={addTrustedDomain}
                  className="btn-primary px-3 py-2 rounded text-sm"
                >
                  Add
                </button>
              </div>
              {config.user.trusted_domains.length === 0 ? (
                <p className="text-xs app-text-muted mt-2">No trusted domains added.</p>
              ) : (
                <div className="mt-2 space-y-2">
                  {config.user.trusted_domains.map((domain) => (
                    <div
                      key={domain}
                      className="flex items-center justify-between app-panel-muted px-3 py-2 rounded-lg"
                    >
                      <span className="text-sm truncate">{domain}</span>
                      <button
                        type="button"
                        onClick={() => removeTrustedDomain(domain)}
                        className="text-xs app-text-danger hover:opacity-80"
                      >
                        Remove
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        ) : null}
      </div>
    </AppDialogPortal>
  );
}
