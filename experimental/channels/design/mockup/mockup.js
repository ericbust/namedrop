const views = [
  ["channel", "Needs you"],
  ["unanswered", "Unanswered request"],
  ["calm", "Calm channel"],
  ["thread", "Thread"],
  ["thread-terminal", "Terminal thread"],
  ["empty", "Empty channel"],
  ["reconnect", "Reconnecting"],
  ["pile", "Variant B: facepile"],
];
const params = new URLSearchParams(location.search);
let view = params.get("view") ?? "channel";
let theme = params.get("theme") ?? "light";
if (params.has("shot")) document.body.dataset.shot = "";

function render() {
  document.documentElement.dataset.theme = theme;
  document.body.dataset.members = view === "pile" ? "pile" : "strip";
  for (const element of document.querySelectorAll("[data-views]"))
    element.hidden = !element.dataset.views.split(" ").includes(view);
  document
    .getElementById("app")
    .classList.toggle("has-thread", view.startsWith("thread"));
  const selected = view === "empty" ? "alpha" : "testbed";
  for (const row of document.querySelectorAll("[data-channel]"))
    if (row.dataset.channel === selected)
      row.setAttribute("aria-current", "page");
    else row.removeAttribute("aria-current");
  for (const button of document.querySelectorAll(".switcher button"))
    button.setAttribute(
      "aria-pressed",
      String(button.dataset.view === view || button.dataset.theme === theme),
    );
}

const switcher = document.querySelector(".switcher");
for (const [id, label] of views) {
  const button = document.createElement("button");
  button.type = "button";
  button.dataset.view = id;
  button.textContent = label;
  button.addEventListener("click", () => {
    view = id;
    render();
  });
  switcher.append(button);
}
for (const id of ["light", "dark"]) {
  const button = document.createElement("button");
  button.type = "button";
  button.dataset.theme = id;
  button.textContent = id === "light" ? "Light" : "Dark";
  button.addEventListener("click", () => {
    theme = id;
    render();
  });
  switcher.append(button);
}
document.addEventListener("keydown", (event) => {
  if (event.key === "t") theme = theme === "light" ? "dark" : "light";
  const index = Number(event.key) - 1;
  if (views[index]) view = views[index][0];
  render();
});
render();
