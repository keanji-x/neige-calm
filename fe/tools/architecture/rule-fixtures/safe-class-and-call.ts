declare function log(value: string): void;
export class Registry {
  static readonly X = 1;
  c = new Map();
}
log('x');
