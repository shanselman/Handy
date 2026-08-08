import React from "react";
import ReactDOM from "react-dom/client";
import ScreenAnnotation from "./ScreenAnnotation";
import "@/i18n";
import "./ScreenAnnotation.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ScreenAnnotation />
  </React.StrictMode>,
);
