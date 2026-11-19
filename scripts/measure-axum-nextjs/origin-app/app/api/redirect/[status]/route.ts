export const dynamic = 'force-dynamic';
function redirect(request: Request, { params }) {
  return new Response(null, {
    status: Number(params.status),
    headers: {
      location: '/api/echo',
      'set-cookie': 'redirect_probe=present; Path=/; HttpOnly; SameSite=Lax',
    },
  });
}
export const POST = redirect;
