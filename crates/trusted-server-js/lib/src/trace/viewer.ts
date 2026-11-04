import { mountTraceViewer } from './report-view';
import './viewer.css';

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', () => mountTraceViewer(), { once: true });
} else {
  mountTraceViewer();
}
