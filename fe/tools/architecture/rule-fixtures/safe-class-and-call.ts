declare function log(value: string): void;
export class Registry {
  static readonly X = 1;
  c = new Map();
  create() { return new Map(); }
}
log('x');
