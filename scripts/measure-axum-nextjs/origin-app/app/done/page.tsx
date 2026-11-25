import { cookies } from 'next/headers';
import Link from 'next/link';
export default function Done({ searchParams }) {
  return (
    <main>
      <h1>Done</h1>
      <p id="token">{searchParams.token}</p>
      <p id="cookie">{cookies().get('probe')?.value || 'none'}</p>
      <Link href="/" prefetch={false}>
        Return
      </Link>
    </main>
  );
}
