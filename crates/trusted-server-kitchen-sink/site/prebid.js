// Lightweight publisher-side placeholder for direct Trusted Server kitchen sink visits.
// Trusted Server's Prebid integration should remove or replace this request and
// inject the TSJS Prebid bundle when the page is served through Trusted Server.
(function () {
  window.pbjs = window.pbjs || {};
  window.pbjs.que = window.pbjs.que || [];
  window.pbjs._trustedServerKitchenSinkStub = true;
})();
