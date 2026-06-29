/** Rail (icon nav) view switching. Toggles .is-shown on the matching .view and
 *  updates the titlebar screen label. EH-0006: switching to the Library or Board route
 *  reloads the store so a run completed in the Workspace appears without a manual
 *  Reload click (both views are otherwise only refreshed on app load + on Run). */
export function wireRail(library, board) {
  const buttons = document.querySelectorAll(".rail__btn");
  const screenTitle = document.getElementById("screenTitle");
  buttons.forEach((btn) => {
    btn.addEventListener("click", () => {
      const route = btn.dataset.route;
      if (!route) return;
      buttons.forEach((b) => b.classList.remove("is-active"));
      btn.classList.add("is-active");
      document.querySelectorAll(".view").forEach((view) => {
        view.classList.toggle("is-shown", view.dataset.view === route);
      });
      if (screenTitle) {
        screenTitle.textContent = route.charAt(0).toUpperCase() + route.slice(1);
      }
      // Refresh the Library and Board from the store whenever they are shown, so a
      // just-finished run lands on tab switch.
      if (route === "library" && library && typeof library.load === "function") {
        library.load();
      }
      if (route === "board" && board && typeof board.load === "function") {
        board.load();
      }
    });
  });
}
