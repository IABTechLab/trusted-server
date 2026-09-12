import { installRenderOwnerInitial } from '../render_journal';
import { registerCurrentFirstDisplayComponent } from '../registration_client';

registerCurrentFirstDisplayComponent('render_owner_initial', installRenderOwnerInitial);
