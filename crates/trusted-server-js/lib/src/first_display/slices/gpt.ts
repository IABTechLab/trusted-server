import { installGptInitial } from '../leaf/gpt_protocol';

import { defineInitialSlice, registerInitialSlice } from './definition';

export const GPT_INITIAL_SLICE = defineInitialSlice('gpt_initial', installGptInitial);

registerInitialSlice(GPT_INITIAL_SLICE, 7);
