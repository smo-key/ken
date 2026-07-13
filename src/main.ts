import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import { theme } from "./lib/theme.svelte";

// Apply the persisted theme before first paint so there's no light-mode flash.
theme.init();

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
