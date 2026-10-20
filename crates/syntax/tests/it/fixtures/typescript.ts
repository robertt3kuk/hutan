// Types and classes
export interface Item {
  readonly id: number;
  name?: string;
}

enum Color { Red, Green }

export class Store<T extends Item> implements Iterable<T> {
  private items: Map<number, T> = new Map();

  constructor(private readonly limit: number = 10) {}

  add(item: T): boolean {
    if (this.items.size >= this.limit) {
      return false;
    }
    this.items.set(item.id, item);
    return true;
  }
}

const greet = (who: string): string => `hello ${who}`;
type Pair = [number, string];
