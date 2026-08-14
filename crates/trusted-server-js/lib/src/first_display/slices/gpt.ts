import { defineInitialSlice, registerInitialSlice } from './definition';

export const GPT_INITIAL_SLICE = defineInitialSlice('gpt_initial');

registerInitialSlice(GPT_INITIAL_SLICE, 7);
