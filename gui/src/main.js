// Ferrum OCR shell. Placeholder: sidebar route switching only. The real view
// wiring (preflight / run_ocr / ocr:// events) lands in later cards; calling
// invoke here would throw since those handlers are not part of this scaffold.

window.addEventListener("DOMContentLoaded", () => {
  const items = document.querySelectorAll(".app__sidebar li");
  const pane = document.getElementById("pane");
  items.forEach((li) => {
    li.addEventListener("click", () => {
      items.forEach((n) => n.classList.remove("is-active"));
      li.classList.add("is-active");
      const route = li.dataset.route;
      pane.innerHTML =
        '<p class="app__placeholder">' + route + " view: not implemented yet.</p>";
    });
  });
});
