// Small TypeScript surface for round-2 multi-language checks.
export class Store {
  private items: string[] = [];
  add(item: string): void {
    this.items.push(item);
  }
  count(): number {
    return this.items.length;
  }
}

export function makeStore(): Store {
  return new Store();
}
