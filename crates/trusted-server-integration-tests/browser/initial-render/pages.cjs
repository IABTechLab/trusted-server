function creative(label) {
  const text = label
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
  return `<!doctype html><html><body style="margin:0;width:300px;height:250px;background:${label === "Server result" ? "#b6e3ff" : "#ffcda8"};font:24px sans-serif"><div class="marker">${text}</div><script>
requestAnimationFrame(() => top.postMessage({
  type: "fixture-creative-ready",
  label: document.querySelector(".marker").textContent,
}, "*"));
</script></body></html>`;
}

function pucPage() {
  return `<!doctype html><script src="/fixture-puc.js"></script><script>
window.ucTag.renderAd(document, {
  adId: new URL(window.location.href).searchParams.get("adId"),
  pubUrl: "https://publisher.example.com",
});
</script>`;
}

module.exports = { creative, pucPage };
