'use server'

import { redirect } from 'next/navigation'

export async function acceptMessage(formData) {
  if (formData.get('message') !== 'native-acceptance') {
    throw new Error('Unexpected synthetic acceptance message')
  }
  redirect('/?action=accepted')
}
