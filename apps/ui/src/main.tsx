import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import "./styles.css";
import { App } from "./App";
import { start } from "./bridge";

createRoot(document.getElementById("root")!).render(<App />);
start();
