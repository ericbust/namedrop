"use strict";
const status = document.getElementById("status");
const out = document.getElementById("out");
const show = (value) => {
  out.textContent = JSON.stringify(value, null, 2);
};
const key = location.pathname.match(/^\/w\/([0-9a-f]{20})$/);
if (key) {
  const events = new EventSource(`/w/${key[1]}/events`);
  events.addEventListener("snapshot", (event) => {
    status.textContent = "Live";
    show(JSON.parse(event.data));
  });
  events.addEventListener("restarting", () => {
    status.textContent = "Updating Namedrop…";
  });
  events.onerror = () => {
    status.textContent = "Disconnected. Run `namedrop dashboard` again.";
  };
} else {
  fetch("/api/spaces")
    .then((response) => response.json())
    .then((spaces) => {
      status.textContent = "Spaces";
      show(spaces);
    });
}
