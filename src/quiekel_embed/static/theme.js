"use strict";

// Runs before the page is drawn: light or dark, from the saved setting the server put on <html>
// ("auto" follows the system), so the app never flashes white when it opens in dark mode.
{
  const root = document.documentElement;
  const mode = root.dataset.themeMode || "auto";
  const dark = mode === "dark" || (mode === "auto" && matchMedia("(prefers-color-scheme: dark)").matches);
  root.dataset.theme = dark ? "dark" : "light";
}
