import { submit } from './actions';
import Link from 'next/link';
export default function Home() {
  return (
    <main>
      <h1>Action probe</h1>
      <form action={submit}>
        <input name="token" defaultValue="probe-initial" />
        <button type="submit">Submit</button>
      </form>
      <Link href="/done?token=none" prefetch={false}>
        Navigate
      </Link>
    </main>
  );
}
