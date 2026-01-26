import { useEffect } from "react";
import { MainLayout } from "./components/layout/MainLayout";
import { useSyncplayStore } from "./store";

function App() {
  const setupEventListeners = useSyncplayStore(
    (state) => state.setupEventListeners
  );

  useEffect(() => {
    setupEventListeners();
  }, [setupEventListeners]);

  return <MainLayout />;
}

export default App;
