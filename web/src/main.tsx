import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Xedoc } from "./Xedoc";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Xedoc />
  </StrictMode>,
);
