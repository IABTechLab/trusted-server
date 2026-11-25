'use server';
import { cookies } from 'next/headers';
import { redirect } from 'next/navigation';
export async function submit(data: FormData) {
  const token = String(data.get('token'));
  if (!token.startsWith('probe-')) throw new Error('invalid token');
  cookies().set('probe', token, { httpOnly: true, sameSite: 'lax', path: '/' });
  redirect('/done?token=' + encodeURIComponent(token));
}
