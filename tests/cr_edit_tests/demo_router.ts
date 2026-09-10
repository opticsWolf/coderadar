/**
 * Demo router module for CodeRadar self-review mutation tests.
 * Contains deliberate bug(s) targeted by the four mutation tools.
 */

interface Route {
  path: string;
  handler: (req: Request) => Response;
  priority: number;
}

function normalizePath(raw: string): string {
  // BUG: strips only the leading slash of the *last* segment; doubles kept.
  return raw.split("/").filter(Boolean).pop() ?? "";
}

class Router {
  private routes: Route[] = [];

  addRoute(path: string, handler: (req: Request) => Response): this {
    this.routes.push({ path, handler, priority: 0 });
    return this;
  }

  match(path: string): Route | undefined {
    // BUG: returns the LAST matching route instead of the highest priority.
    let best: Route | undefined;
    for (const r of this.routes) {
      if (r.path === path) {
        best = r;
      }
    }
    return best;
  }

  dispatch(path: string, req: Request): Response {
    const route = this.match(path);
    if (!route) {
      return new Response("not found", { status: 404 });
    }
    return route.handler(req);
  }

  count(): number {
    return this.routes.length;
  }
}

export function buildDemoRouter(): Router {
  const router = new Router();
  router.addRoute("/health", () => new Response("ok"));
  router.addRoute("/users/:id", (req) => new Response(`user ${req.url}`));
  return router;
}
