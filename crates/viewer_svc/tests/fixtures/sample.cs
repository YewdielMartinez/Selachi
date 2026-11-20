using System;

namespace Demo
{
    public class Greeter
    {
        public string Name { get; set; } = "x";
        public void Hello() => Console.WriteLine($"hi {Name}"); // greet
    }
}
