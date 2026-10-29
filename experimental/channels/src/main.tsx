import { createRoot } from "react-dom/client";
import { App } from "./ui/App.tsx";
import "./style.css";

const root = document.getElementById("root");
if (root) createRoot(root).render(<App />);
