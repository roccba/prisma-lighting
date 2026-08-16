import MainWindow from "./MainWindow";

// M8: the app boots into the main operation window (the live show). The M7 patch
// editor is preserved as `Editor.tsx` and returns as a secondary window opened from
// this window's menu in a later step (M8-SPEC §6); it is not the app's root anymore.
export default function App() {
  return <MainWindow />;
}
