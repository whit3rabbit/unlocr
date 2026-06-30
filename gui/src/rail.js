/** Rail (icon nav) view switching. Toggles .is-shown on the matching .view,
 *  updates the titlebar screen label, and (EH-0001) moves keyboard focus into the
 *  newly shown view + announces the route name so a screen-reader user gets a
 *  signal they navigated. Focus + announce run only inside this click handler, so
 *  they never fire on a programmatic load. EH-0006: switching to the Library or
 *  Board route reloads the store so a run completed in the Workspace appears
 *  without a manual Reload click (both views are otherwise only refreshed on app
 *  load + on Run). */
export function wireRail(library, board) {
  const buttons = document.querySelectorAll(".rail__btn");
  const screenTitle = document.getElementById("screenTitle");
  const announcer = document.getElementById("routeAnnouncer");
  const routeLabel = (route) => route.charAt(0).toUpperCase() + route.slice(1);
  buttons.forEach((btn) => {
    btn.addEventListener("click", () => {
      const route = btn.dataset.route;
      if (!route) return;
      buttons.forEach((b) => {
        b.classList.remove("is-active");
        b.removeAttribute("aria-current");
      });
      btn.classList.add("is-active");
      btn.setAttribute("aria-current", "page");
      let shown = null;
      document.querySelectorAll(".view").forEach((view) => {
        const isMatch = view.dataset.view === route;
        view.classList.toggle("is-shown", isMatch);
        if (isMatch) shown = view;
      });
      if (screenTitle) {
        screenTitle.textContent = routeLabel(route);
      }
      // Announce the route via the polite live region. A changed route re-fires;
      // re-clicking the active route is a no-op, so no stale repeat announcement.
      if (announcer) {
        announcer.textContent = routeLabel(route);
      }
      // Move keyboard focus into the shown view (tabindex=-1 makes it focusable
      // without joining the Tab order). Programmatic focus does not trigger
      // :focus-visible, so no ring flashes on the whole view; the next real Tab
      // lands on the first control inside it.
      if (shown && typeof shown.focus === "function") {
        shown.focus({ preventScroll: true });
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
