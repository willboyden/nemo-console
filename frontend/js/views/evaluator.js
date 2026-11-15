import { NcView } from '../components/base.js';
import { genericSystemView } from '../components/generic-system.js';
import { EVALUATOR } from '../run-descriptors.js';
import { evaluatorDetail } from '../runviz/evaluator-detail.js';
// NeMo Evaluator: offline benchmark suites. Scores (bench, model) are a table, not a time series; the History tab (addendum 9.15) draws them with confidence
// intervals, as a model x benchmark matrix, against cost, and with their failure rates.
const DESC = { system: 'evaluator', charts: [], tables: ['ev.score'], runHistory: { ...EVALUATOR, detail: evaluatorDetail } };
class NcEvaluator extends NcView {
  setup() {
    genericSystemView(this, DESC);
  }
}
customElements.define('nc-evaluator', NcEvaluator);
