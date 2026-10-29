"use strict";

// The secret comes in the URL fragment, which browsers never send to a server.
const KEY = "zerodb-studio";
const match = /^#k=([0-9a-f]{32})$/.exec(location.hash);
if (match) {
  sessionStorage.setItem(KEY, match[1]);
  history.replaceState(null, "", location.pathname + location.search);
}

const out = document.getElementById("info");
fetch("/api/info", { headers: { "X-Zerodb-Studio": sessionStorage.getItem(KEY) || "" } })
  .then(async (response) => {
    const body = await response.text();
    out.textContent = response.ok ? body : `${response.status}: ${body}`;
  })
  .catch((error) => {
    out.textContent = String(error);
  });
