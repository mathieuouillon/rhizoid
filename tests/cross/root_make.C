// Writes, with ROOT itself, objects of every class rhizoid models, filled
// with data: TH1/TH2/TH3 in every cell type (C, S, I, L, F, D), TProfile,
// TProfile2D, TProfile3D, TGraph, TGraphErrors, TGraphAsymmErrors, TF1,
// TNamed, TObjString, plus a TH1D with a fitted function in fFunctions and
// an empty TH1D. Deterministic (TRandom3).
//
// The file is both a fixture (tests/data/root_hists.root) and the source
// of the class descriptions rhizoid embeds (tests/cross/capture_streamers.py).
//
//   root -b -q -l 'tests/cross/root_make.C("root_hists.root")'

#include <TF1.h>
#include <TFile.h>
#include <TGraph.h>
#include <TGraphAsymmErrors.h>
#include <TGraphErrors.h>
#include <TH1.h>
#include <TH2.h>
#include <TH3.h>
#include <TNamed.h>
#include <TObjString.h>
#include <TProfile.h>
#include <TProfile2D.h>
#include <TProfile3D.h>
#include <TRandom3.h>

void root_make(const char *path)
{
   TFile f(path, "RECREATE");
   TRandom3 r(42);

   // One-dimensional, every cell type.
   auto h1d = new TH1D("h1d", "a TH1D;x [GeV];entries", 40, -4, 4);
   h1d->Sumw2();
   const double fedges[] = {0.0, 0.1, 0.5, 1.0, 2.5, 4.0};
   auto h1f = new TH1F("h1f", "variable bins", 5, fedges);
   auto h1i = new TH1I("h1i", "integers", 10, 0, 10);
   auto h1c = new TH1C("h1c", "chars", 6, 0, 6);
   auto h1s = new TH1S("h1s", "shorts", 6, 0, 6);
   auto h1l = new TH1L("h1l", "longs", 6, 0, 6);

   // Two-dimensional.
   const double yedges[] = {-1.0, 0.0, 0.5, 3.0};
   auto h2d = new TH2D("h2d", "a TH2D;x;y", 10, -2, 2, 3, yedges);
   auto h2f = new TH2F("h2f", "weighted TH2F", 8, 0, 8, 8, 0, 8);
   auto h2i = new TH2I("h2i", "TH2I", 4, 0, 4, 3, 0, 3);
   auto h2c = new TH2C("h2c", "TH2C", 3, 0, 3, 2, 0, 2);
   auto h2s = new TH2S("h2s", "TH2S", 3, 0, 3, 2, 0, 2);
   auto h2l = new TH2L("h2l", "TH2L", 3, 0, 3, 2, 0, 2);

   // Three-dimensional.
   auto h3d = new TH3D("h3d", "a TH3D;x;y;z", 4, -2, 2, 3, -2, 2, 2, -2, 2);
   h3d->Sumw2();
   auto h3f = new TH3F("h3f", "TH3F", 3, 0, 3, 3, 0, 3, 3, 0, 3);
   auto h3i = new TH3I("h3i", "TH3I", 2, 0, 2, 2, 0, 2, 2, 0, 2);
   auto h3c = new TH3C("h3c", "TH3C", 2, 0, 2, 2, 0, 2, 2, 0, 2);
   auto h3s = new TH3S("h3s", "TH3S", 2, 0, 2, 2, 0, 2, 2, 0, 2);
   auto h3l = new TH3L("h3l", "TH3L", 2, 0, 2, 2, 0, 2, 2, 0, 2);

   // Profiles.
   auto p1 = new TProfile("p1", "a TProfile;x;<y>", 10, -2, 2);
   auto p1s = new TProfile("p1s", "spread errors", 5, 0, 5, -10, 10, "s");
   auto p2 = new TProfile2D("p2", "a TProfile2D", 4, -2, 2, 3, -2, 2);
   auto p3 = new TProfile3D("p3", "a TProfile3D", 2, -2, 2, 2, -2, 2, 2, -2, 2);

   auto fitted = new TH1D("fitted", "with a fit", 50, -5, 5);
   new TH1D("empty", "nothing filled", 10, 0, 1);

   for (int i = 0; i < 5000; ++i) {
      double x = r.Gaus(), y = r.Gaus(), z = r.Gaus();
      double w = r.Uniform(0.5, 1.5);
      h1d->Fill(x, w);
      h1f->Fill(r.Exp(1.0));
      h1i->Fill(r.Integer(12));
      h1c->Fill(r.Integer(7));
      h1s->Fill(r.Integer(7));
      h1l->Fill(r.Integer(7));
      h2d->Fill(x, r.Uniform(-1.5, 3.5));
      h2f->Fill(r.Uniform(-1, 9), r.Uniform(0, 8), 2.0);
      h2i->Fill(r.Uniform(0, 4), r.Uniform(0, 3));
      h2c->Fill(r.Uniform(0, 3), r.Uniform(0, 2));
      h2s->Fill(r.Uniform(0, 3), r.Uniform(0, 2));
      h2l->Fill(r.Uniform(0, 3), r.Uniform(0, 2));
      h3d->Fill(x, y, z, w);
      h3f->Fill(r.Uniform(0, 3), r.Uniform(0, 3), r.Uniform(0, 3));
      h3i->Fill(r.Uniform(0, 2), r.Uniform(0, 2), r.Uniform(0, 2));
      h3c->Fill(r.Uniform(0, 2), r.Uniform(0, 2), r.Uniform(0, 2));
      h3s->Fill(r.Uniform(0, 2), r.Uniform(0, 2), r.Uniform(0, 2));
      h3l->Fill(r.Uniform(0, 2), r.Uniform(0, 2), r.Uniform(0, 2));
      p1->Fill(x, 2 * x + y, w);
      p1s->Fill(r.Uniform(0, 5), y * 3);
      p2->Fill(x, y, x * y + z);
      p3->Fill(x, y, z, x + y + z, w);
      fitted->Fill(r.Gaus(0.5, 1.5));
   }
   fitted->Fit("gaus", "Q0");

   // Graphs.
   const int n = 6;
   double gx[n], gy[n], ex[n], ey[n], exl[n], exh[n], eyl[n], eyh[n];
   for (int i = 0; i < n; ++i) {
      gx[i] = 0.5 * i;
      gy[i] = r.Gaus(i, 0.3);
      ex[i] = 0.05;
      ey[i] = r.Uniform(0.1, 0.4);
      exl[i] = 0.04;
      exh[i] = 0.06;
      eyl[i] = r.Uniform(0.1, 0.2);
      eyh[i] = r.Uniform(0.2, 0.3);
   }
   TGraph g(n, gx, gy);
   g.SetNameTitle("g", "a TGraph;x;y");
   g.Write("g");
   TGraphErrors ge(n, gx, gy, ex, ey);
   ge.SetNameTitle("ge", "a TGraphErrors");
   ge.Write("ge");
   TGraphAsymmErrors ga(n, gx, gy, exl, exh, eyl, eyh);
   ga.SetNameTitle("ga", "a TGraphAsymmErrors");
   ga.Write("ga");

   // A function on its own.
   TF1 line("line", "[0]*x+[1]", -1, 7);
   line.SetParameters(1.5, -0.25);
   line.SetParError(0, 0.1);
   line.Write("line");

   TNamed named("named", "a TNamed");
   named.Write("named");
   TObjString s("written by ROOT");
   s.Write("s");
   f.Write();
   f.Close();
}
