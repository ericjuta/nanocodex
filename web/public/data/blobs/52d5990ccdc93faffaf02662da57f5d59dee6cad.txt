const json = (body: unknown, init?: ResponseInit) =>
  Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      ...init?.headers,
    },
  });

export default {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);

    if (url.pathname === "/api/health" && request.method === "GET") {
      return json({
        service: "nanocodex",
        runtime: "cloudflare-workers",
        status: "ok",
      });
    }

    if (url.pathname === "/api/proposals" && request.method === "POST") {
      return json(
        {
          status: "payment_required",
          mode: "testnet_preview",
          amount: "0.20",
          currency: "USD",
          message: "A live MPP challenge will replace this preview response.",
        },
        { status: 402 },
      );
    }

    return json({ error: "not_found" }, { status: 404 });
  },
};
