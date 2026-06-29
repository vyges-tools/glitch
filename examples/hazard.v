// f = a*b + a'*c : a static-1 hazard on `a` reconverging at f.
module hazard(a, b, c, f);
  input  a, b, c;
  output f;
  wire   an, t1, t2;
  INV  i1(.A(a),  .Y(an));
  AND2 g1(.A(a),  .B(b), .Z(t1));
  AND2 g2(.A(an), .B(c), .Z(t2));
  OR2  g3(.A(t1), .B(t2), .Z(f));
endmodule
