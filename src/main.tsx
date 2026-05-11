import ReactDOM from "react-dom/client";
import App from "./App";

// StrictMode 비활성 — dev 에서 effect 가 2번 실행되면 tmux 세션이 중복
// attach + auto-run claude 가 2번 발사돼서 입력이 섞임. 실제 동작 디버깅 위해 끔.
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <App />,
);
