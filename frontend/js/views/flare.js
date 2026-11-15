import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { flareExtras } from '../flare/extras.js';
// NVIDIA Flare: NVIDIA's research-prototype inference server on the lab's sm120 GPUs (the RTX PRO 6000s, the
// AITop100's RTX 5090) and sm121 GPUs (the DGX Sparks, one or all four). Every number is per target; the engine
// counters exist only while a target serves. Since addendum 9.16 the page's own cards (js/flare/extras.js) draw the target grid, the token and
// request rates per target, prefill vs generated vs cache-reused tokens and active vs pending requests; the generic charts keep what those
// do not cover (targets serving, speculative-draft acceptance).
const DESC = {
  system: 'flare',
  stats: ['flare.targets_serving'], // the other numbers are per target: the target grid and the cards below carry them, and the all-values table keeps every one
  charts: ['flare.targets_serving', 'flare.draft_acceptance'],
  extras: (scope, shared) => flareExtras(scope, shared).el,
};
class NcFlare extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-flare', NcFlare);
