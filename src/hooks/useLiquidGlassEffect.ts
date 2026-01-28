import { useEffect, useRef } from "react";
import {
  GlassMaterialVariant,
  isGlassSupported,
  setLiquidGlassEffect,
} from "tauri-plugin-liquid-glass-api";
import { Effect, EffectState, getCurrentWindow } from "@tauri-apps/api/window";

type Params = {
  reduceTransparency: boolean;
};

export function useLiquidGlassEffect({ reduceTransparency }: Params) {
  const supportedRef = useRef<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;

    const apply = async () => {
      try {
        const window = getCurrentWindow();
        if (reduceTransparency) {
          if (supportedRef.current === null) {
            supportedRef.current = await isGlassSupported();
          }
          if (supportedRef.current) {
            await setLiquidGlassEffect({ enabled: false });
          }
          await window.setEffects({ effects: [] });
          return;
        }

        if (supportedRef.current === null) {
          supportedRef.current = await isGlassSupported();
        }
        if (cancelled) {
          return;
        }
        if (supportedRef.current) {
          await window.setEffects({ effects: [] });
          await setLiquidGlassEffect({
            enabled: true,
            cornerRadius: 16,
            variant: GlassMaterialVariant.Regular,
          });
          return;
        }

        const userAgent = navigator.userAgent ?? "";
        const isMac = userAgent.includes("Macintosh");
        const isLinux = userAgent.includes("Linux");
        if (!isMac && !isLinux) {
          return;
        }
        await window.setEffects({
          effects: [Effect.HudWindow],
          state: EffectState.Active,
          radius: 16,
        });
      } catch {
        if (cancelled) {
          return;
        }
      }
    };

    void apply();

    return () => {
      cancelled = true;
    };
  }, [reduceTransparency]);
}
