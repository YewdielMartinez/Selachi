interface User {
  name: string;
  age: number;
}

export function greet(user: User): string {
  const msg = `hello ${user.name}`; // greeting
  return msg;
}

class Counter<T extends number> {
  private count = 0;
  inc(by: T): void {
    this.count += by;
  }
}
