import { useEffect } from "react";
import { MainLayout } from "./components/layout/MainLayout";
import { useSyncplayStore } from "./store";

function App() {
  const setupEventListeners = useSyncplayStore((state) => state.setupEventListeners);

  useEffect(() => {
    setupEventListeners();
  }, [setupEventListeners]);

  useEffect(() => {
    const handleContextMenu = (event: MouseEvent) => {
      event.preventDefault();
    };

    document.addEventListener("contextmenu", handleContextMenu);
    return () => {
      document.removeEventListener("contextmenu", handleContextMenu);
    };
  }, []);

  return <MainLayout />;
}

export default App;
