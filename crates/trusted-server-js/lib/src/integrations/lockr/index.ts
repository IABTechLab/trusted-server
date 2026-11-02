import { installLockrApiGuard } from './api_guard';
import { installLockrGuard } from './script_guard';

if (typeof window !== 'undefined') {
  installLockrApiGuard();
  installLockrGuard();
}
