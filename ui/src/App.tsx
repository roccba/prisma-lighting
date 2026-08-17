import { getCurrentWindow } from "@tauri-apps/api/window";
import Editor from "./Editor";
import MainWindow from "./MainWindow";

// M8: the app boots into the main operation window (the live show). The M7 patch editor
// opens as a SECONDARY window in the same process (M8-SPEC §6/§7), from the main window's
// menu. Both windows load this same bundle; they are told apart by the window label the
// Rust side assigns ("editor" for the editor window, "main" for the operation window).
const isEditorWindow = getCurrentWindow().label === "editor";

export default function App() {
  return isEditorWindow ? <Editor /> : <MainWindow />;
}
